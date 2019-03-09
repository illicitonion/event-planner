Event planner
=============

I like to go do things (concerts, theatre, lectures, ...), and I prefer having company at them! I often have a long list of things I'd like to maybe do, and want to put out vague invitations to see what people are interested in.

I used to do this on Facebook, but increasingly both I, and my friends, are using Facebook less.

This is a simple web app to help find overlapping interest among folks with low commitment. It doesn't actually organise anything, it just helps to signal desire/intent.

Structure
---------

Each event has a unique URL. These can be shared in any usual way. People can express intent by writing down their name. Each time this happens, an email will be triggered to the site owner. That's it.

Running instructions
--------------------

Everything is built with cargo. First, create a database:
```
cargo install diesel_cli --no-default-features --features sqlite
diesel migration run
```

Then build the binary (requires sqlite3 to be installed, and usable as a library):
```
cargo build --release
```

Make a settings.json (or settings.yaml, or anything the `config` crate understands) like:
```
{
  "port": 8080,
  "host": "example.com",
  "organiser_name": "MyName",
  "db_path": "db.sqlite",
  "insert_password": "some_password",
  "templates_dir": "templates",
  "mailgun_from_name": "Event Planner",
  "mailgun_from_email_prefix": "notifications",
  "mailgun_api_key": "your-mailgun-api-key",
  "notify_email": "someone@example.com"
}
```

And run:
```
./target/release/event-planner
```
