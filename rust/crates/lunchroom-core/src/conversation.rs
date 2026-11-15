//! The conversations of the console (spec §17 item 10, "The conversation panel").
//!
//! §17.10: "Conversations are kept by session identity (I1.1) and shown by name: where two sessions
//! have had one name, each session's conversation is its own, labelled with the name and its
//! harness. A line of Lunchroom's that names one agent with no pair, such as a YOLO line for an
//! agent that has exchanged no message, has a conversation of that agent alone, listed with that
//! agent's name only. ... A line of Lunchroom's that names one agent shows in every pair that
//! includes that agent, and a line that names three or more agents shows in every pair of the
//! agents it names. ... The lines stand in time order: a message at the time it was sent, a line of
//! Lunchroom's at the time of its event, equal times in the order recorded. Lunchroom removes no
//! message and no line of Lunchroom's from the store (§16 item 1), so the panel shows the whole
//! conversation of that pair since the store was made."
//!
//! This module only arranges what the store holds; it reads nothing and removes nothing.

use std::collections::{BTreeMap, BTreeSet};

/// One agent: a session of a harness, with the name it has.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Party {
    /// The harness's word (`Claude Code`, `Codex`, `OpenClaw`, `Hermes`).
    pub harness: String,
    /// The harness's session id.
    pub session: String,
    /// The name the user gave it, as last seen.
    pub name: String,
}

impl Party {
    /// What names the session across names and renames (I1.1).
    #[must_use]
    pub fn id(&self) -> (String, String) {
        (self.harness.clone(), self.session.clone())
    }
}

/// A message the store holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageIn {
    pub id: String,
    /// When it was sent.
    pub at: u64,
    /// Where it stands among everything recorded, messages and lines (one sequence).
    pub order: u64,
    pub from: Party,
    pub to: Party,
    pub text: String,
}

/// A line of Lunchroom's the store holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIn {
    /// What names it across snapshots (its number among the lines).
    pub id: u64,
    /// The time of its event.
    pub at: u64,
    /// Where it stands among everything recorded, messages and lines (one sequence).
    pub order: u64,
    pub text: String,
    /// The agents it names.
    pub agents: Vec<Party>,
}

/// What one entry of a conversation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Message,
    Line,
}

/// One line of a conversation panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What names it across snapshots.
    pub key: String,
    pub kind: EntryKind,
    pub at: u64,
    /// For a message, the sender's name as the panel labels it, and the recipient's.
    pub from: String,
    pub to: String,
    /// The whole text.
    pub text: String,
}

/// The conversation of one pair of agents, or of one agent alone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conversation {
    /// What names it across snapshots: the sessions' identities.
    pub key: String,
    /// The one or two agents.
    pub parties: Vec<Party>,
    /// The names as shown: with the harness where a name is shared by two sessions.
    pub label: String,
    pub entries: Vec<Entry>,
}

impl Conversation {
    /// The time of the last entry.
    #[must_use]
    pub fn last(&self) -> u64 {
        self.entries.iter().map(|e| e.at).max().unwrap_or(0)
    }
}

type Id = (String, String);

fn key_of(ids: &[&Id]) -> String {
    ids.iter()
        .map(|(h, s)| format!("{}:{}:{h}{s}", h.len(), s.len()))
        .collect::<Vec<_>>()
        .join("|")
}

/// How `p` is labelled: its name, with its harness where another session has the same name, and
/// a piece of its session id where that does not tell them apart.
fn label_of(p: &Party, parties: &BTreeMap<Id, Party>) -> String {
    let same_name: Vec<&Party> = parties
        .values()
        .filter(|q| q.name == p.name && q.id() != p.id())
        .collect();
    if same_name.is_empty() {
        return p.name.clone();
    }
    let same_harness = same_name.iter().any(|q| q.harness == p.harness);
    if same_harness {
        let short: String = p.session.chars().take(8).collect();
        format!("{} ({}, {short})", p.name, p.harness)
    } else {
        format!("{} ({})", p.name, p.harness)
    }
}

/// Every conversation, the most recent first (then by key). Entries stand in time order: a message
/// by the time it was sent, a line by the time of its event; equal times in the order recorded
/// (`order` is one sequence over messages and lines; a tie of both goes to the message).
#[must_use]
#[allow(clippy::too_many_lines)] // one pass: the sessions, the pairs, then each pair's entries
pub fn conversations(messages: &[MessageIn], lines: &[LineIn]) -> Vec<Conversation> {
    // Every session seen, with the name it was last seen under.
    let mut parties: BTreeMap<Id, (u64, Party)> = BTreeMap::new();
    let mut see = |p: &Party, at: u64, order: u64| {
        let stamp = at
            .saturating_mul(1 << 20)
            .saturating_add(order.min((1 << 20) - 1));
        let slot = parties.entry(p.id()).or_insert((stamp, p.clone()));
        if stamp >= slot.0 {
            *slot = (stamp, p.clone());
        }
    };
    for m in messages {
        see(&m.from, m.at, m.order);
        see(&m.to, m.at, m.order);
    }
    for l in lines {
        for p in &l.agents {
            see(p, l.at, l.order);
        }
    }
    let parties: BTreeMap<Id, Party> = parties.into_iter().map(|(k, (_, p))| (k, p)).collect();

    // The pairs: of the messages, and of each line that names two agents or more.
    let mut pairs: BTreeSet<Vec<Id>> = BTreeSet::new();
    let pair = |a: &Id, b: &Id| -> Vec<Id> {
        let mut v = vec![a.clone(), b.clone()];
        v.sort();
        v.dedup();
        v
    };
    for m in messages {
        pairs.insert(pair(&m.from.id(), &m.to.id()));
    }
    let named = |l: &LineIn| -> Vec<Id> {
        let set: BTreeSet<Id> = l.agents.iter().map(Party::id).collect();
        set.into_iter().collect()
    };
    for l in lines {
        let ids = named(l);
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                pairs.insert(pair(a, b));
            }
        }
    }
    // A line that names one agent with no pair: a conversation of that agent alone.
    for l in lines {
        if let [only] = named(l).as_slice()
            && !pairs.iter().any(|p| p.contains(only))
        {
            pairs.insert(vec![only.clone()]);
        }
    }

    let mut out: Vec<Conversation> = Vec::new();
    for ids in &pairs {
        let mut raw: Vec<(u64, u8, u64, Entry)> = Vec::new();
        let label_party = |id: &Id| {
            parties
                .get(id)
                .map_or_else(String::new, |p| label_of(p, &parties))
        };
        for m in messages {
            let (a, b) = (m.from.id(), m.to.id());
            if pair(&a, &b) == *ids {
                raw.push((
                    m.at,
                    0,
                    m.order,
                    Entry {
                        key: format!("m:{}", m.id),
                        kind: EntryKind::Message,
                        at: m.at,
                        from: label_party(&a),
                        to: label_party(&b),
                        text: m.text.clone(),
                    },
                ));
            }
        }
        for l in lines {
            let named = named(l);
            let shown = match named.as_slice() {
                [] => false,
                // One agent: every pair that includes it, or its own conversation.
                [one] => ids.contains(one),
                // Two or more: every pair of the agents it names.
                many => ids.len() == 2 && ids.iter().all(|i| many.contains(i)),
            };
            if shown {
                raw.push((
                    l.at,
                    1,
                    l.order,
                    Entry {
                        key: format!("l:{}", l.id),
                        kind: EntryKind::Line,
                        at: l.at,
                        from: String::new(),
                        to: String::new(),
                        text: l.text.clone(),
                    },
                ));
            }
        }
        raw.sort_by_key(|r| (r.0, r.2, r.1));
        let members: Vec<Party> = ids.iter().filter_map(|i| parties.get(i).cloned()).collect();
        let mut shown_names: Vec<String> = ids.iter().map(label_party).collect();
        shown_names.sort();
        let label = shown_names.join(" and ");
        out.push(Conversation {
            key: key_of(&ids.iter().collect::<Vec<_>>()),
            parties: members,
            label,
            entries: raw.into_iter().map(|r| r.3).collect(),
        });
    }
    out.sort_by(|a, b| b.last().cmp(&a.last()).then_with(|| a.key.cmp(&b.key)));
    out
}

#[cfg(test)]
mod tests;
