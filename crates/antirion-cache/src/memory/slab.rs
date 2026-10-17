//! The slots of the memory backend and the recency lists that order them: one list over every slot
//! (the global least-recently-used order) and one per entry cap. Lists are intrusive and doubly
//! linked, so adding, touching, removing and taking the oldest are constant time.

use std::sync::Arc;

use crate::entry::Entry;

pub(super) type Id = u32;

/// The list every slot is on.
pub(super) const GLOBAL: u32 = 0;

#[derive(Clone, Copy, Debug)]
struct Link {
    list: u32,
    prev: Option<Id>,
    next: Option<Id>,
}

/// One stored entry and its use.
#[derive(Debug)]
pub(super) struct Slot {
    pub(super) key: Arc<str>,
    pub(super) entry: Arc<Entry>,
    pub(super) weight: u64,
    pub(super) hits: u64,
    pub(super) last_hit_ms: Option<i64>,
    links: Vec<Link>,
}

impl Slot {
    pub(super) fn new(key: Arc<str>, entry: Arc<Entry>, weight: u64) -> Slot {
        Slot {
            key,
            entry,
            weight,
            hits: 0,
            last_hit_ms: None,
            links: Vec::new(),
        }
    }

    fn link(&self, list: u32) -> Option<Link> {
        self.links.iter().find(|l| l.list == list).copied()
    }

    fn link_mut(&mut self, list: u32) -> Option<&mut Link> {
        self.links.iter_mut().find(|l| l.list == list)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Head {
    head: Option<Id>,
    tail: Option<Id>,
    len: usize,
}

/// Slots by id, with their lists.
#[derive(Debug)]
pub(super) struct Slab {
    slots: Vec<Option<Slot>>,
    free: Vec<Id>,
    heads: Vec<Head>,
}

impl Default for Slab {
    fn default() -> Slab {
        Slab {
            slots: Vec::new(),
            free: Vec::new(),
            heads: vec![Head::default()],
        }
    }
}

impl Slab {
    pub(super) fn get(&self, id: Id) -> Option<&Slot> {
        self.slots.get(id as usize).and_then(Option::as_ref)
    }

    pub(super) fn get_mut(&mut self, id: Id) -> Option<&mut Slot> {
        self.slots.get_mut(id as usize).and_then(Option::as_mut)
    }

    /// Stores `slot` at the back of the global list.
    pub(super) fn insert(&mut self, slot: Slot) -> Id {
        let id = match self.free.pop() {
            Some(id) => {
                if let Some(cell) = self.slots.get_mut(id as usize) {
                    *cell = Some(slot);
                }
                id
            }
            None => {
                self.slots.push(Some(slot));
                Id::try_from(self.slots.len() - 1).unwrap_or(Id::MAX)
            }
        };
        self.push_back(GLOBAL, id);
        id
    }

    /// A new, empty list.
    pub(super) fn new_list(&mut self) -> u32 {
        self.heads.push(Head::default());
        u32::try_from(self.heads.len() - 1).unwrap_or(u32::MAX)
    }

    pub(super) fn len(&self, list: u32) -> usize {
        self.heads.get(list as usize).map_or(0, |h| h.len)
    }

    /// The least recently used slot of `list`.
    pub(super) fn front(&self, list: u32) -> Option<Id> {
        self.heads.get(list as usize).and_then(|h| h.head)
    }

    /// Puts `id` at the back of `list`; nothing when it is on it already.
    pub(super) fn push_back(&mut self, list: u32, id: Id) {
        let Some(tail) = self.heads.get(list as usize).map(|h| h.tail) else {
            return;
        };
        let Some(slot) = self.get_mut(id) else {
            return;
        };
        if slot.link(list).is_some() {
            return;
        }
        slot.links.push(Link {
            list,
            prev: tail,
            next: None,
        });
        if let Some(t) = tail
            && let Some(l) = self.get_mut(t).and_then(|s| s.link_mut(list))
        {
            l.next = Some(id);
        }
        if let Some(h) = self.heads.get_mut(list as usize) {
            if h.head.is_none() {
                h.head = Some(id);
            }
            h.tail = Some(id);
            h.len += 1;
        }
    }

    fn unlink(&mut self, list: u32, id: Id) {
        let Some(link) = self.get(id).and_then(|s| s.link(list)) else {
            return;
        };
        match link
            .prev
            .and_then(|p| self.get_mut(p))
            .and_then(|s| s.link_mut(list))
        {
            Some(p) => p.next = link.next,
            None => {
                if let Some(h) = self.heads.get_mut(list as usize) {
                    h.head = link.next;
                }
            }
        }
        match link
            .next
            .and_then(|n| self.get_mut(n))
            .and_then(|s| s.link_mut(list))
        {
            Some(n) => n.prev = link.prev,
            None => {
                if let Some(h) = self.heads.get_mut(list as usize) {
                    h.tail = link.prev;
                }
            }
        }
        if let Some(h) = self.heads.get_mut(list as usize) {
            h.len = h.len.saturating_sub(1);
        }
        if let Some(s) = self.get_mut(id) {
            s.links.retain(|l| l.list != list);
        }
    }

    /// Makes `id` the most recently used of every list it is on.
    pub(super) fn touch(&mut self, id: Id) {
        let lists: Vec<u32> = self
            .get(id)
            .map(|s| s.links.iter().map(|l| l.list).collect())
            .unwrap_or_default();
        for list in lists {
            self.unlink(list, id);
            self.push_back(list, id);
        }
    }

    /// Takes `id` off every list and out of the slab.
    pub(super) fn remove(&mut self, id: Id) -> Option<Slot> {
        let lists: Vec<u32> = self.get(id)?.links.iter().map(|l| l.list).collect();
        for list in lists {
            self.unlink(list, id);
        }
        let slot = self.slots.get_mut(id as usize)?.take();
        if slot.is_some() {
            self.free.push(id);
        }
        slot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(k: &str) -> Slot {
        let entry = Entry {
            written_at_ms: 0,
            ttl_ms: 1,
            content_type: String::new(),
            body: None,
            events: None,
            provider: String::new(),
            deployment: String::new(),
            model: String::new(),
            upstream_status: 200,
            upstream_dialect: None,
            usage: None,
            meta: None,
        };
        Slot::new(Arc::from(k), Arc::new(entry), 1)
    }

    fn order(s: &Slab, list: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = s.front(list);
        while let Some(id) = cur {
            let slot = s.get(id).unwrap();
            out.push(slot.key.to_string());
            cur = slot.link(list).unwrap().next;
        }
        out
    }

    #[test]
    fn lists_keep_recency_order() {
        let mut s = Slab::default();
        let cap = s.new_list();
        let a = s.insert(slot("a"));
        let b = s.insert(slot("b"));
        let c = s.insert(slot("c"));
        s.push_back(cap, a);
        s.push_back(cap, c);
        assert_eq!(order(&s, GLOBAL), ["a", "b", "c"]);
        s.touch(a);
        assert_eq!(order(&s, GLOBAL), ["b", "c", "a"]);
        assert_eq!(order(&s, cap), ["c", "a"]);
        assert_eq!(s.remove(c).unwrap().key.as_ref(), "c");
        assert_eq!(order(&s, GLOBAL), ["b", "a"]);
        assert_eq!(order(&s, cap), ["a"]);
        assert_eq!(s.len(cap), 1);
        let d = s.insert(slot("d"));
        assert_eq!(d, c, "freed ids are reused");
        assert_eq!(order(&s, GLOBAL), ["b", "a", "d"]);
        s.remove(b);
        s.remove(a);
        s.remove(d);
        assert_eq!(s.len(GLOBAL), 0);
        assert!(s.front(GLOBAL).is_none() && s.front(cap).is_none());
    }
}
