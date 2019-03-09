use crate::models::{Event, InterestedPerson};
#[macro_use]
extern crate diesel;
use diesel::prelude::*;
use futures::{Future, Stream};
use gotham::handler::HandlerFuture;
use gotham::helpers::http::response::{create_empty_response, create_response};
use gotham::router::{builder::*, Router};
use gotham::state::{FromState, State};
use gotham_derive::{StateData, StaticResponseExtender};
use handlebars::Handlebars;
use hyper::StatusCode;
use lazy_static::lazy_static;
use serde_derive::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use url::form_urlencoded;
use uuid::Uuid;

mod models;
mod schema;

#[derive(Deserialize)]
struct Config {
    port: u32,
    host: String,
    organiser_name: String,
    db_path: PathBuf,
    templates_dir: PathBuf,
    notify_email: String,
    mailgun_from_name: String,
    mailgun_from_email_prefix: String,
    mailgun_api_key: String,
    insert_password: String,
}

lazy_static! {
    static ref CONFIG: Config = {
        let mut settings = config::Config::default();
        settings.merge(config::File::with_name("settings")).unwrap();
        settings.try_into::<Config>().unwrap()
    };
    static ref HTTP_CLIENT: reqwest::Client = reqwest::Client::new();
}

fn main() {
    gotham::start(format!("127.0.0.1:{}", CONFIG.port), || Ok(router()));
}

fn router() -> Router {
    build_simple_router(|route| {
        route.get("/").to(serve_index);
        route
            .get("/event/:event_uuid")
            .with_path_extractor::<EventContext>()
            .to(serve_event);
        route.post("/interested").to(mark_interested);

        route.get("/event/create").to(create_event_page);
        route.post("/event/create").to(do_create_event);
    })
}

#[derive(Default, Serialize)]
struct InterestedParties {
    named: Vec<String>,
    unnamed: usize,
    any_interested: bool,
    unnamed_plurality: &'static str,
}

fn connect() -> Result<SqliteConnection, Error> {
    SqliteConnection::establish(&format!("{}", CONFIG.db_path.display()))
        .map_err(Error::DatabaseConnection)
}

fn serve_event(state: State) -> (State, hyper::Response<hyper::Body>) {
    let event_context = EventContext::borrow_from(&state);

    let response = match event_context.render() {
        Ok(body) => create_response(&state, StatusCode::OK, mime::TEXT_HTML_UTF_8, body),
        Err(err) => err.as_response(&state),
    };

    (state, response)
}

fn render_template<Values: serde::Serialize>(
    template_filename: &str,
    values: &Values,
) -> Result<String, Error> {
    let mut templates = Handlebars::new();
    templates.set_strict_mode(true);
    templates
        .register_template_file("template", CONFIG.templates_dir.join(template_filename))
        .map_err(Error::TemplateError)?;

    templates
        .render("template", &values)
        .map_err(Error::TemplateRenderError)
}

#[derive(Deserialize, StateData, StaticResponseExtender)]
struct EventContext {
    event_uuid: Uuid,
}

impl EventContext {
    fn render(&self) -> Result<String, Error> {
        let (mut event, interested_parties) = self.find_event_and_interested_parties()?;
        event.description = event.description.replace("\n", "<br />");

        let values = json!({
            "event": event,
            "other_interested": interested_parties,
            "organiser_name": CONFIG.organiser_name,
        });

        render_template("event.html", &values)
    }

    fn find_event(&self, conn: &SqliteConnection) -> Result<Event, Error> {
        use self::schema::events::dsl::{events, uuid};
        events
            .filter(uuid.eq(format!("{}", self.event_uuid)))
            .first(conn)
            .map_err(|err| match err {
                diesel::result::Error::NotFound => Error::EventNotFound(self.event_uuid),
                err => Error::Database(err),
            })
    }

    fn find_event_and_interested_parties(&self) -> Result<(Event, InterestedParties), Error> {
        let conn = connect()?;

        use self::schema::interested_persons::dsl::{event_id, interested_persons};

        self.find_event(&conn).and_then(|event| {
            let people = interested_persons
                .filter(event_id.eq(event.id))
                .load::<InterestedPerson>(&conn)
                .map_err(Error::Database)?;
            let mut ips = InterestedParties::default();
            for person in people {
                ips.any_interested = true;
                if person.displayed {
                    ips.named.push(person.name);
                } else {
                    ips.unnamed += 1;
                }
            }
            if ips.unnamed == 1 {
                ips.unnamed_plurality = "person";
            } else {
                ips.unnamed_plurality = "people";
            }
            Ok((event, ips))
        })
    }
}

struct InterestedContext {
    name: String,
    show_name: bool,
    event_uuid: String,
}

impl InterestedContext {
    fn from_form_body(buf: bytes::Bytes) -> Result<InterestedContext, Error> {
        let mut form_data = form_urlencoded::parse(&buf)
            .into_owned()
            .collect::<HashMap<_, _>>();
        match (form_data.remove("name"), form_data.remove("event_uuid")) {
            (Some(name), Some(event_uuid)) => Ok(InterestedContext {
                name,
                show_name: form_data
                    .get("show_name")
                    .map(|value| value.as_str() == "true")
                    .unwrap_or(false),
                event_uuid,
            }),
            (Some(_name), None) => Err(Error::MissingFieldError(vec!["event_id".to_owned()])),
            (None, Some(_event_id)) => Err(Error::MissingFieldError(vec!["name".to_owned()])),
            (None, None) => Err(Error::MissingFieldError(vec![
                "event_id".to_owned(),
                "name".to_owned(),
            ])),
        }
    }
}

#[derive(Debug)]
enum Error {
    EventNotFound(uuid::Uuid),
    MissingFieldError(Vec<String>),
    MailgunError(reqwest::Error),
    TemplateError(handlebars::TemplateFileError),
    TemplateRenderError(handlebars::RenderError),
    DatabaseConnection(diesel::ConnectionError),
    Database(diesel::result::Error),
    WrongPassword,
    Inner(Box<std::error::Error>),
}

impl Error {
    fn as_response(&self, state: &State) -> http::Response<hyper::Body> {
        create_response(
            &state,
            self.status_code(),
            mime::TEXT_PLAIN_UTF_8,
            format!("Error: {}", self),
        )
    }

    fn status_code(&self) -> StatusCode {
        use Error::*;
        match self {
            EventNotFound(..) => StatusCode::NOT_FOUND,
            MissingFieldError(..) => StatusCode::BAD_REQUEST,
            WrongPassword => StatusCode::UNAUTHORIZED,
            MailgunError(..)
            | TemplateError(..)
            | TemplateRenderError(..)
            | DatabaseConnection(..)
            | Database(..)
            | Inner(..) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        use Error::*;
        match self {
            EventNotFound(event_id) => write!(f, "Event not found: {}", event_id),
            MissingFieldError(fields) => write!(
                f,
                "Missing field{}: {}",
                if fields.len() == 1 { "" } else { "s" },
                fields.join(", ")
            ),
            MailgunError(..) => write!(f, "Error occurred sending email"),
            TemplateError(..) => write!(f, "Template error"),
            TemplateRenderError(..) => write!(f, "Template render error"),
            DatabaseConnection(..) => write!(f, "Database connection error"),
            Database(..) => write!(f, "Database error"),
            WrongPassword => write!(f, "Wrong password"),
            Inner(..) => write!(f, "Unexpected error"),
        }
    }
}

fn mark_interested(mut state: State) -> Box<HandlerFuture> {
    let f = hyper::Body::take_from(&mut state)
        .concat2()
        .then(|body| match body {
            Ok(body) => match mark_interested_inner(body.into_bytes()) {
                Ok(event_id) => {
                    let response = redirect(&state, &format!("/event/{}", event_id));
                    Ok((state, response))
                }
                Err(err) => {
                    let response = err.as_response(&state);
                    Ok((state, response))
                }
            },
            Err(err) => {
                let response = Error::Inner(Box::new(err)).as_response(&state);
                Ok((state, response))
            }
        });
    Box::new(f)
}

fn mark_interested_inner(body: bytes::Bytes) -> Result<String, Error> {
    let interested_context = InterestedContext::from_form_body(body)?;
    let name = interested_context.name.clone();
    let event_uuid = interested_context.event_uuid.clone();

    let event = {
        let conn = connect()?;

        let event_context = EventContext {
            event_uuid: event_uuid
                .parse()
                .map_err(|err| Error::Inner(Box::new(err)))?,
        };
        let event = event_context.find_event(&conn)?;

        use self::schema::interested_persons::dsl::{
            event_id, interested_persons, name, show_name,
        };
        use diesel::dsl::insert_into;

        insert_into(interested_persons)
            .values((
                name.eq(interested_context.name),
                show_name.eq(interested_context.show_name),
                event_id.eq(event.id),
            ))
            .execute(&conn)
            .map_err(Error::Database)?;
        event
    };

    mailgun_send(&name, &event.title, &event_uuid)?;
    Ok(event_uuid)
}

fn mailgun_send(name: &str, event_title: &str, event_uuid: &str) -> Result<(), Error> {
    let mut map = HashMap::new();
    map.insert(
        "from",
        format!(
            "{} <{}@{}>",
            CONFIG.mailgun_from_name, CONFIG.mailgun_from_email_prefix, CONFIG.host
        ),
    );
    map.insert("to", CONFIG.notify_email.to_owned());
    map.insert(
        "subject",
        format!("Someone is interested in {}", event_title),
    );
    map.insert(
        "text",
        format!("{} is interested in {}: {}", name, event_title, event_uuid),
    );
    HTTP_CLIENT
        .post(&format!(
            "https://api.mailgun.net/v3/{}/messages",
            CONFIG.host
        ))
        .basic_auth("api", Some(&CONFIG.mailgun_api_key))
        .query(&map)
        .send()
        .map_err(Error::MailgunError)?
        .error_for_status()
        .map(|_| ())
        .map_err(Error::MailgunError)
}

fn create_event_page(state: State) -> (State, hyper::Response<hyper::Body>) {
    let response = match render_template("create.html", &Vec::<String>::new()) {
        Ok(body) => create_response(&state, StatusCode::OK, mime::TEXT_HTML_UTF_8, body),
        Err(err) => err.as_response(&state),
    };

    (state, response)
}

fn do_create_event(mut state: State) -> Box<HandlerFuture> {
    let f = hyper::Body::take_from(&mut state)
        .concat2()
        .then(|body| match body {
            Ok(body) => {
                let response = match do_create_event_inner(body.into_bytes()) {
                    Ok(uuid) => redirect(&state, &format!("{}", uuid)),
                    Err(err) => err.as_response(&state),
                };
                Ok((state, response))
            }
            Err(err) => {
                let response = Error::Inner(Box::new(err)).as_response(&state);
                Ok((state, response))
            }
        });
    Box::new(f)
}

fn do_create_event_inner(buf: bytes::Bytes) -> Result<Uuid, Error> {
    #[derive(Deserialize)]
    struct EventCreation {
        title: String,
        link: String,
        date_time: String,
        description: String,
        password: String,
    }

    let event_data = serde_urlencoded::from_bytes::<EventCreation>(&buf)
        .map_err(|err| Error::Inner(Box::new(err)))?;

    if event_data.password != CONFIG.insert_password {
        return Err(Error::WrongPassword);
    }

    let generated_uuid = Uuid::new_v4();
    let generated_uuid_string = format!("{}", generated_uuid);

    use self::schema::events::dsl::*;
    use diesel::dsl::insert_into;

    let conn = connect()?;
    insert_into(events)
        .values((
            uuid.eq(generated_uuid_string),
            title.eq(event_data.title),
            link.eq(event_data.link),
            date_time.eq(event_data.date_time),
            description.eq(event_data.description),
        ))
        .execute(&conn)
        .map_err(Error::Database)?;
    Ok(generated_uuid)
}

fn redirect(state: &State, to: &str) -> hyper::Response<hyper::Body> {
    let mut response = create_empty_response(&state, StatusCode::SEE_OTHER);
    response
        .headers_mut()
        .insert(hyper::header::LOCATION, to.parse().unwrap());
    response
}

fn serve_index(state: State) -> (State, hyper::Response<hyper::Body>) {
    let mut variables = HashMap::new();
    variables.insert("organiser_name", CONFIG.organiser_name.as_str());
    let response = match render_template("index.html", &variables) {
        Ok(body) => create_response(&state, StatusCode::OK, mime::TEXT_HTML_UTF_8, body),
        Err(err) => err.as_response(&state),
    };

    (state, response)
}