//! Čistá logika stavu aplikace (bez UI), aby šla testovat.

use std::collections::HashMap;

use tymy_core::{
    AccountKind, Conversation, ConversationId, ConversationKind, Message, MessageId, Profile,
    Reaction, UserId,
};

/// Chaty rozdělené na připnuté (v ručním pořadí) a ostatní (od poslední aktivity;
/// nové chaty bez aktivity nahoře).
pub fn order_chats<'a>(
    chats: impl IntoIterator<Item = &'a Conversation>,
    pins: &[ConversationId],
) -> (Vec<&'a Conversation>, Vec<&'a Conversation>) {
    let (mut pinned, mut rest): (Vec<_>, Vec<_>) =
        chats.into_iter().partition(|c| pins.contains(&c.id));
    pinned.sort_by_key(|c| pins.iter().position(|p| p == &c.id));
    rest.sort_by_key(|c| {
        std::cmp::Reverse(
            c.last_activity
                .map(|t| t.timestamp_millis())
                .unwrap_or(i64::MAX),
        )
    });
    (pinned, rest)
}

/// Posune připnutý chat o `delta` míst (záporné = nahoru).
pub fn move_pin(pins: &mut [ConversationId], id: &ConversationId, delta: isize) {
    let Some(i) = pins.iter().position(|p| p == id) else {
        return;
    };
    let j = (i as isize + delta).clamp(0, pins.len() as isize - 1) as usize;
    if i != j {
        let item = pins[i].clone();
        if i < j {
            pins[i..=j].rotate_left(1);
        } else {
            pins[j..=i].rotate_right(1);
        }
        pins[j] = item;
    }
}

/// Tým s kanály pro záložku Týmy.
pub struct TeamGroup<'a> {
    pub name: String,
    pub channels: Vec<&'a Conversation>,
}

/// Seskupí kanály podle týmu. Týmy podle názvu, v týmu obecný kanál první, pak podle názvu.
pub fn group_teams<'a>(convs: impl IntoIterator<Item = &'a Conversation>) -> Vec<TeamGroup<'a>> {
    let mut groups: Vec<(ConversationId, TeamGroup<'a>)> = Vec::new();
    for c in convs {
        let Some(team) = &c.team else { continue };
        let idx = match groups.iter().position(|(id, _)| id == &team.id) {
            Some(i) => i,
            None => {
                groups.push((
                    team.id.clone(),
                    TeamGroup {
                        name: String::new(),
                        channels: vec![],
                    },
                ));
                groups.len() - 1
            }
        };
        let g = &mut groups[idx].1;
        if let Some(name) = &team.name {
            g.name = name.clone();
        }
        g.channels.push(c);
    }
    let mut out: Vec<TeamGroup<'a>> = groups
        .into_iter()
        .map(|(id, mut g)| {
            if g.name.is_empty() {
                g.name = "Tým".into();
            }
            g.channels
                .sort_by_key(|c| (c.id != id, channel_name(c).to_lowercase()));
            g
        })
        .collect();
    out.sort_by_key(|g| g.name.to_lowercase());
    out
}

/// Název kanálu; kořen týmu (bez tématu) je obecný kanál.
pub fn channel_name(c: &Conversation) -> String {
    c.topic.clone().unwrap_or_else(|| "Obecný".into())
}

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

    /// Jméno známé z jiného zdroje (např. z hledání lidí) – pro nový 1:1 chat.
    pub fn set(&mut self, id: &ConversationId, name: &str) {
        let list = self.names.entry(id.clone()).or_default();
        list.retain(|n| n != name);
        list.insert(0, name.to_owned());
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

/// Moje MRI (pro místní zápis reakcí před potvrzením serverem).
pub fn my_mri(me: &Profile) -> UserId {
    match me.kind {
        AccountKind::Work => UserId(format!("8:orgid:{}", me.account_id)),
        AccountKind::Personal => UserId(format!("8:live:{}", me.account_id)),
    }
}

/// Přepne moji reakci `key` na zprávě; vrátí `true`, když jsem ji přidal.
/// Moje reakce poznám podle konce MRI (`account`), stejně jako [`is_me`].
pub fn toggle_reaction(m: &mut Message, key: &str, me: &UserId, account: &str) -> bool {
    let mine = |u: &UserId| u.as_str().ends_with(account);
    if let Some(i) = m.reactions.iter().position(|r| r.key == key) {
        let r = &mut m.reactions[i];
        if r.users.iter().any(mine) {
            r.users.retain(|u| !mine(u));
            if r.users.is_empty() {
                m.reactions.remove(i);
            }
            return false;
        }
        r.users.push(me.clone());
        return true;
    }
    m.reactions.push(Reaction {
        key: key.to_owned(),
        users: vec![me.clone()],
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tymy_core::MessageKind;

    fn me() -> Profile {
        Profile {
            account_id: "oid-me".into(),
            kind: AccountKind::Work,
            display_name: Some("Já".into()),
            username: None,
            tenant_id: None,
        }
    }

    #[test]
    fn toggles_my_reaction() {
        let me = UserId("8:orgid:me".into());
        let mut m = msg("1", "x", "X");
        m.reactions = vec![Reaction {
            key: "like".into(),
            users: vec![UserId("8:orgid:other".into())],
        }];
        assert!(toggle_reaction(&mut m, "like", &me, "me"));
        assert_eq!(m.reactions[0].users.len(), 2);
        assert!(!toggle_reaction(&mut m, "like", &me, "me"));
        assert_eq!(m.reactions[0].users.len(), 1);
        assert!(toggle_reaction(&mut m, "heart", &me, "me"));
        assert!(!toggle_reaction(&mut m, "heart", &me, "me"));
        assert_eq!(m.reactions.len(), 1, "prázdná reakce zmizí");
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
            team: None,
        }
    }

    fn at(id: &str, ms: Option<i64>) -> Conversation {
        let mut c = conv(None);
        c.id = id.into();
        c.last_activity =
            ms.and_then(|ms| chrono::TimeZone::timestamp_millis_opt(&chrono::Utc, ms).single());
        c
    }

    #[test]
    fn pinned_first_in_manual_order_rest_by_activity() {
        let convs = [
            at("a", Some(1)),
            at("b", Some(3)),
            at("c", Some(2)),
            at("new", None),
            at("p", Some(0)),
        ];
        let pins: Vec<ConversationId> = vec!["p".into(), "a".into(), "gone".into()];
        let (pinned, rest) = order_chats(&convs, &pins);
        let ids = |v: &[&Conversation]| v.iter().map(|c| c.id.0.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&pinned), ["p", "a"]);
        assert_eq!(ids(&rest), ["new", "b", "c"]);
    }

    #[test]
    fn move_pin_up_and_down_with_bounds() {
        let mut pins: Vec<ConversationId> = vec!["a".into(), "b".into(), "c".into()];
        move_pin(&mut pins, &"c".into(), -1);
        assert_eq!(pins, ["a", "c", "b"].map(ConversationId::from));
        move_pin(&mut pins, &"a".into(), -1);
        assert_eq!(pins, ["a", "c", "b"].map(ConversationId::from));
        move_pin(&mut pins, &"a".into(), 5);
        assert_eq!(pins, ["c", "b", "a"].map(ConversationId::from));
    }

    #[test]
    fn groups_channels_by_team() {
        let team = |c: &mut Conversation, root: &str, name: Option<&str>| {
            c.kind = ConversationKind::Channel;
            c.team = Some(tymy_core::TeamRef {
                id: root.into(),
                name: name.map(Into::into),
            });
        };
        let mut root = at("r1", None);
        team(&mut root, "r1", Some("BOOTIQ"));
        let mut ch = at("c1", None);
        ch.topic = Some("Bootiq-crew".into());
        team(&mut ch, "r1", None);
        let mut orphan = at("c2", None);
        orphan.topic = Some("Kanál".into());
        team(&mut orphan, "r2", None);
        let groups = group_teams([&ch, &orphan, &root]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "BOOTIQ");
        let names: Vec<_> = groups[0].channels.iter().map(|c| channel_name(c)).collect();
        assert_eq!(names, ["Obecný", "Bootiq-crew"]);
        assert_eq!(groups[1].name, "Tým");
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
