table! {
    events (id) {
        id -> Integer,
        uuid -> Text,
        title -> Text,
        link -> Text,
        description -> Text,
        date_time -> Text,
    }
}

table! {
    interested_persons (id) {
        id -> Integer,
        event_id -> Integer,
        name -> Text,
        show_name -> Bool,
    }
}

allow_tables_to_appear_in_same_query!(events, interested_persons,);
