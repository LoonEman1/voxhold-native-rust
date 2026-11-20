//! Shared personal-conversation labels. The R04 inbox popup was retired with
//! W08a: the "Сообщения" sidebar is the only recents surface, but both it and
//! the message index keep using the same title/peer rules from Go
//! `recents.go::conversationTile`.

use voxhold_api::Conversation;

/// Go `conversationTile`: an explicit group name wins, a direct chat uses the
/// other member, and a nameless multi-person chat falls back to "Беседа".
pub fn conversation_title(conversation: &Conversation, self_user_id: i64) -> String {
    let mut names = Vec::new();
    for person in &conversation.members {
        if self_user_id > 0 && person.user_id == self_user_id {
            continue;
        }
        names.push(person.username.clone());
    }
    if !conversation.name.is_empty() {
        return conversation.name.clone();
    }
    if names.is_empty() {
        return "Беседа".into();
    }
    names.join(", ")
}

pub fn conversation_peer(conversation: &Conversation, self_user_id: i64) -> i64 {
    if !conversation.direct {
        return 0;
    }
    conversation
        .members
        .iter()
        .find(|person| person.user_id != self_user_id)
        .map(|person| person.user_id)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxhold_api::Author;

    fn conversation(name: &str, direct: bool, members: Vec<(i64, &str)>) -> Conversation {
        Conversation {
            id: 7,
            name: name.into(),
            direct,
            members: members
                .into_iter()
                .map(|(user_id, username)| Author {
                    user_id,
                    username: username.into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn direct_and_group_titles_follow_go_recents() {
        let direct = conversation("", true, vec![(1, "me"), (2, "neva")]);
        assert_eq!(conversation_title(&direct, 1), "neva");
        assert_eq!(conversation_peer(&direct, 1), 2);

        let group = conversation("", false, vec![(1, "me"), (2, "neva"), (3, "alex")]);
        assert_eq!(conversation_title(&group, 1), "neva, alex");
        assert_eq!(conversation_peer(&group, 1), 0);

        let named = conversation("Проект", true, vec![(1, "me"), (2, "neva")]);
        assert_eq!(conversation_title(&named, 1), "Проект");

        let empty = conversation("", false, vec![(1, "me")]);
        assert_eq!(conversation_title(&empty, 1), "Беседа");
    }
}
