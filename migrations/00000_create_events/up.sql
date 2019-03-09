CREATE TABLE events(
  id INTEGER PRIMARY KEY NOT NULL,
  uuid VARCHAR(255) NOT NULL,
  title TEXT NOT NULL,
  link TEXT NOT NULL,
  description TEXT NOT NULL,
  date_time TEXT NOT NULL -- Text is best date format!
);

CREATE TABLE interested_persons(
  id INTEGER PRIMARY KEY NOT NULL,
  event_id INTEGER NOT NULL,
  name TEXT NOT NULL,
  show_name BOOLEAN NOT NULL
);
