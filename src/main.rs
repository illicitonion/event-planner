pub use crate::models::{Event, InterestedPerson};
#[macro_use]
extern crate diesel;
use diesel::prelude::*;
use futures::{Future, Stream};
use gotham::handler::HandlerFuture;
use gotham::helpers::http::response::{create_empty_response, create_response};
use gotham::router::{builder::*, Router};
use gotham::state::{FromState, State};
use gotham_derive::{StateData, StaticResponseExtender};
use hyper::StatusCode;
use lazy_static::lazy_static;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use url::form_urlencoded;
use uuid::Uuid;

mod models;
mod schema;

include!(concat!(env!("OUT_DIR"), "/templates.rs"));

#[derive(Deserialize)]
struct Config {
    port: u32,
    host: String,
    organiser_name: String,
    db_path: PathBuf,
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
            .get("/event/:event_uuids")
            .with_path_extractor::<EventsContext>()
            .to(serve_events);

        route
            .get("/events/:event_uuids")
            .with_path_extractor::<EventsContext>()
            .to(serve_events);

        route.post("/interested").to(mark_interested);

        route.get("/event/create").to(create_event_page);
        route.post("/event/create").to(do_create_event);
    })
}

#[derive(Default, Serialize)]
pub struct InterestedParties {
    named: Vec<String>,
    unnamed: usize,
    any_interested: bool,
    unnamed_plurality: &'static str,
}

fn connect() -> Result<SqliteConnection, Error> {
    SqliteConnection::establish(&format!("{}", CONFIG.db_path.display()))
        .map_err(Error::DatabaseConnection)
}

fn serve_events(state: State) -> (State, hyper::Response<hyper::Body>) {
    let event_context = EventsContext::borrow_from(&state);

    let response = match event_context.render() {
        Ok(body) => create_response(&state, StatusCode::OK, mime::TEXT_HTML_UTF_8, body),
        Err(err) => err.as_response(&state),
    };

    (state, response)
}

#[derive(Deserialize, StateData, StaticResponseExtender)]
struct EventsContext {
    #[serde(with = "serde_with::rust::StringWithSeparator::<serde_with::CommaSeparator>")]
    event_uuids: Vec<Uuid>,
}

impl EventsContext {
    fn render(&self) -> Result<Vec<u8>, Error> {
        match &self.event_uuids[..] {
            [] => Err(Error::MissingFieldError(vec!["event id".to_owned()])),
            [event_uuid] => {
                let (event, interested_parties) =
                    Self::find_event_and_interested_parties(*event_uuid)?;
                let event_description = templates::Html(event.description.replace("\n", "<br />"));
                let mut buf = Vec::new();
                templates::event(
                    &mut buf,
                    CONFIG.organiser_name.as_str(),
                    &event,
                    &event_description,
                    &interested_parties,
                )
                .unwrap();
                Ok(buf)
            }
            event_uuids => {
                let events: Result<Vec<_>, _> = event_uuids
                    .iter()
                    .map(|event_uuid| {
                        let (event, interested_parties) =
                            Self::find_event_and_interested_parties(*event_uuid)?;
                        let description =
                            templates::Html(event.description.replace("\n", "<br />"));
                        Ok(EventData {
                            event,
                            description,
                            interested_parties,
                        })
                    })
                    .collect();
                let mut buf = Vec::new();
                templates::events(&mut buf, CONFIG.organiser_name.as_str(), &events?).unwrap();
                Ok(buf)
            }
        }
    }

    fn find_event(conn: &SqliteConnection, event_uuid: Uuid) -> Result<Event, Error> {
        use self::schema::events::dsl::{events, uuid};
        events
            .filter(uuid.eq(format!("{}", event_uuid)))
            .first(conn)
            .map_err(|err| match err {
                diesel::result::Error::NotFound => Error::EventNotFound(event_uuid),
                err => Error::Database(err),
            })
    }

    fn find_event_and_interested_parties(
        event_uuid: Uuid,
    ) -> Result<(Event, InterestedParties), Error> {
        let conn = connect()?;

        use self::schema::interested_persons::dsl::{event_id, interested_persons};

        Self::find_event(&conn, event_uuid).and_then(|event| {
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

pub struct EventData {
    event: Event,
    description: templates::Html<String>,
    interested_parties: InterestedParties,
}

struct InterestedContext {
    name: String,
    show_name: bool,
    event_uuids: Vec<String>,
}

impl InterestedContext {
    fn from_form_body(buf: bytes::Bytes) -> Result<InterestedContext, Error> {
        let mut form_data = form_urlencoded::parse(&buf)
            .into_owned()
            .collect::<HashMap<_, _>>();
        let (name, show_name) = if let Some(name) = form_data.remove("name") {
            let show_name = form_data
                .get("show_name")
                .map(|value| value.as_str() == "true")
                .unwrap_or(false);
            (name, show_name)
        } else {
            return Err(Error::MissingFieldError(vec!["name".to_owned()]));
        };
        let event_uuids = form_data
            .drain()
            .filter(|(key, value)| key.starts_with("event-") && value == "true")
            .map(|(key, _)| key[6..].to_owned())
            .collect();
        Ok(InterestedContext {
            name,
            show_name,
            event_uuids,
        })
    }
}

#[derive(Debug)]
enum Error {
    EventNotFound(uuid::Uuid),
    MissingFieldError(Vec<String>),
    MailgunError(reqwest::Error),
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
            MailgunError(..) | DatabaseConnection(..) | Database(..) | Inner(..) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
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
                Ok(event_ids) => {
                    let response = if event_ids.len() == 1 {
                        redirect(&state, &format!("/event/{}", event_ids[0]))
                    } else {
                        redirect(&state, &format!("/events/{}", event_ids.join(",")))
                    };

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

fn mark_interested_inner(body: bytes::Bytes) -> Result<Vec<String>, Error> {
    let interested_context = InterestedContext::from_form_body(body)?;
    let name = interested_context.name.clone();
    for event_uuid in interested_context.event_uuids.iter().cloned() {
        let event = {
            let conn = connect()?;

            let event = EventsContext::find_event(
                &conn,
                event_uuid
                    .parse()
                    .map_err(|err| Error::Inner(Box::new(err)))?,
            )?;

            use self::schema::interested_persons::dsl::{
                event_id, interested_persons, name, show_name,
            };
            use diesel::dsl::insert_into;

            insert_into(interested_persons)
                .values((
                    name.eq(interested_context.name.clone()),
                    show_name.eq(interested_context.show_name.clone()),
                    event_id.eq(event.id),
                ))
                .execute(&conn)
                .map_err(Error::Database)?;
            event
        };

        mailgun_send(&name, &event.title, &event_uuid)?;
    }
    Ok(interested_context.event_uuids)
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
    let response = create_response(
        &state,
        StatusCode::OK,
        mime::TEXT_HTML_UTF_8,
        templates::statics::create_html.content,
    );

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
    let mut buf = Vec::new();
    templates::index(&mut buf, CONFIG.organiser_name.as_str()).unwrap();
    let response = create_response(&state, StatusCode::OK, mime::TEXT_HTML_UTF_8, buf);
    (state, response)
}
