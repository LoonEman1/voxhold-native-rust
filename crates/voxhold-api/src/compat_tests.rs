use super::*;

#[test]
fn null_arrays_match_go_zero_values() {
    let message: Message =
        serde_json::from_str(r#"{"id":1,"attachments":null,"reactions":null}"#).unwrap();
    assert!(message.attachments.is_empty());
    assert!(message.reactions.is_empty());
    let channel: Channel = serde_json::from_str(r#"{"id":1,"name":null}"#).unwrap();
    assert!(channel.name.is_empty());
}

#[test]
fn conversation_rows_keep_peer_and_muted_fields() {
    let conversation: Conversation = serde_json::from_str(
        r#"{"id":7,"name":"","direct":true,"saved":false,"muted":true,
            "last_message_id":12,"unread_count":2,
            "members":[{"user_id":1,"username":"me"},{"user_id":2,"username":"neva"}]}"#,
    )
    .unwrap();
    assert!(conversation.direct && conversation.muted);
    assert_eq!(conversation.last_message_id, 12);
    assert_eq!(conversation.members[1].user_id, 2);
}
