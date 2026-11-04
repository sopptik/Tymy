//! Čistá logika stavu aplikace (bez UI), aby šla testovat.

use std::collections::HashMap;

use tymy_core::{Conversation, ConversationId, ConversationKind, Message, MessageId, Profile};

/// Kdo jsem: podle MRI (obsahuje object id) nebo, u osobních účtů, podle jména.
pub fn is_me(me: &Profile, m: &Message) -> bool {
    if let Some(sender) = &m.sender
        && sender.as_str().ends_with(me.account_id.as_str())
    {
        return true;
    }
    matches!((&me.display_name, &m.sender_name), (Some(a), Some(b)) if a == b)
}

fn numeric(id: &MessageId) -> u64 {
    id.as_str().parse().unwrap_or(0)
}

/// Sloučí nové zprávy do existujících (od nejstarší). Stejné id se nahradí novější verzí.
pub fn merge_messages(existing: &mut Vec<Message>, new: Vec<Message>) {
    let mut index: HashMap<MessageId, usize> = existing
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.clone(), i))
        .collect();
    for m in new {
        match index.get(&m.id) {
            Some(&i) => existing[i] = m,
            None => {
                index.insert(m.id.clone(), existing.len());
                existing.push(m);
            }
        }
    }
    existing.sort_by_key(|m| numeric(&m.id));
}

/// Jména ostatních účastníků, která jsme viděli ve zprávách (od posledního).
#[derive(Default)]
pub struct PeerNames {
    names: HashMap<ConversationId, Vec<String>>,
}

impl PeerNames {
    /// Zapamatuje si odesílatele zpráv (kromě mě); `messages` od nejstarší.
    pub fn learn<'a>(&mut self, me: &Profile, messages: impl IntoIterator<Item = &'a Message>) {
        for m in messages {
            if is_me(me, m) || !m.is_user_content() {
                continue;
            }
            let Some(name) = m.sender_name.as_deref().filter(|n| !n.is_empty()) else {
                continue;
            };
            let list = self.names.entry(m.conversation_id.clone()).or_default();
            list.retain(|n| n != name);
            list.insert(0, name.to_owned());
        }
    }

    pub fn knows(&self, id: &ConversationId) -> bool {
        self.names.get(id).is_some_and(|n| !n.is_empty())
    }

    /// Název chatu: téma, jinak jména ostatních (max. 3).
    pub fn title(&self, c: &Conversation) -> String {
        if let Some(t) = c.topic.as_deref().filter(|t| !t.trim().is_empty()) {
            return t.to_owned();
        }
        match self.names.get(&c.id) {
            Some(names) if !names.is_empty() => {
                let shown: Vec<_> = names.iter().take(3).map(String::as_str).collect();
                let mut s = shown.join(", ");
                if names.len() > 3 {
                    s.push_str(&format!(" +{}", names.len() - 3));
                }
                s
            }
            _ => match c.kind {
                ConversationKind::Meeting => "Schůzka".into(),
                ConversationKind::Group => "Skupinový chat".into(),
                _ => "Chat".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tymy_core::{AccountKind, MessageKind, UserId};

    fn me() -> Profile {
        Profile {
            account_id: "oid-me".into(),
            kind: AccountKind::Work,
            display_name: Some("Já".into()),
            username: None,
            tenant_id: None,
        }
    }

    fn msg(id: &str, sender: &str, name: &str) -> Message {
        Message {
            id: id.into(),
            conversation_id: "c1".into(),
            client_message_id: None,
            sender: Some(UserId(format!("8:orgid:{sender}"))),
            sender_name: Some(name.into()),
            kind: MessageKind::RichText,
            content: format!("<p>{id}</p>"),
            composed_at: None,
            arrived_at: None,
            edited_at: None,
            deleted: false,
            attachments: vec![],
            reactions: vec![],
        }
    }

    fn conv(topic: Option<&str>) -> Conversation {
        Conversation {
            id: "c1".into(),
            kind: ConversationKind::OneOnOne,
            topic: topic.map(Into::into),
            last_message: None,
            last_activity: None,
            read_up_to: None,
        }
    }

    #[test]
    fn merge_dedupes_and_sorts_numerically() {
        let mut v = vec![msg("100", "a", "A"), msg("900", "a", "A")];
        let mut edited = msg("100", "a", "A");
        edited.content = "upraveno".into();
        merge_messages(
            &mut v,
            vec![msg("1000", "b", "B"), edited, msg("500", "b", "B")],
        );
        let ids: Vec<_> = v.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["100", "500", "900", "1000"]);
        assert_eq!(v[0].content, "upraveno");
    }

    #[test]
    fn recognizes_me() {
        assert!(is_me(&me(), &msg("1", "oid-me", "Kdokoli")));
        assert!(is_me(&me(), &msg("1", "jiny", "Já")));
        assert!(!is_me(&me(), &msg("1", "jiny", "Alice")));
    }

    #[test]
    fn title_uses_topic_then_peers_excluding_me() {
        let mut p = PeerNames::default();
        assert_eq!(p.title(&conv(None)), "Chat");
        p.learn(
            &me(),
            &[
                msg("1", "a", "Alice"),
                msg("2", "oid-me", "Já"),
                msg("3", "b", "Bob"),
                msg("4", "a", "Alice"),
            ],
        );
        // nejnovější první, bez duplicit a beze mě
        assert_eq!(p.title(&conv(None)), "Alice, Bob");
        assert_eq!(p.title(&conv(Some("Projekt"))), "Projekt");
    }
}
