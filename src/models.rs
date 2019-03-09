use crate::schema::*;

use serde_derive::Serialize;

#[derive(Debug, Identifiable, Queryable, Serialize)]
pub struct Event {
    pub id: i32,
    pub uuid: String,
    pub title: String,
    pub link: String,
    pub description: String,
    pub date_time: String,
}

#[derive(Associations, Debug, Identifiable, Queryable, Serialize)]
#[belongs_to(Event)]
pub struct InterestedPerson {
    pub id: i32,
    pub event_id: i32,
    pub name: String,
    pub displayed: bool,
}
