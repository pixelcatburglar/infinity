//! Bounded in-memory replay fences for trusted native combat attempts.
//! The client has no corresponding protocol; native owners register sources and
//! reserve attempts in launch order. No database or persistent replay is provided.
//!
//! Attempt nodes are recycled once the completed prefix is folded. A trusted
//! native owner must prevent re-registration of a retired activation.

use std::collections::HashMap;

use super::Refusal;

struct Source {
    generation: u64,
    first: u64,
    last: Option<u64>,
    head: Option<usize>,
    tail: Option<usize>,
    retired: bool,
    subject: Option<Option<i64>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State { Reserved, Claimed, Completed }

struct Node {
    sequence: u64,
    next: Option<usize>,
    state: State,
}

/// Live or retiring sources and outstanding attempt nodes are independently
/// limited to `capacity`. Memory is allocated at construction.
pub(super) struct Replay {
    capacity: usize,
    sources: HashMap<[u8; 16], Source>,
    nodes: Vec<Option<Node>>,
    free: Vec<usize>,
    attempts: HashMap<([u8; 16], u64), usize>,
}

impl Replay {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            sources: HashMap::with_capacity(capacity),
            nodes: Vec::with_capacity(capacity),
            free: Vec::with_capacity(capacity),
            attempts: HashMap::with_capacity(capacity),
        }
    }

    pub(super) fn register(&mut self, activation: [u8; 16], generation: u64, first_sequence: u64) -> Result<(), Refusal> {
        if u64::from_be_bytes(activation[8..].try_into().expect("fixed activation")) != generation {
            return Err(Refusal::Invalid);
        }
        if let Some(source) = self.sources.get(&activation) {
            return if source.generation == generation && source.first == first_sequence && !source.retired {
                Ok(())
            } else { Err(Refusal::Invalid) };
        }
        if self.sources.len() == self.capacity { return Err(Refusal::Full); }
        self.sources.insert(activation, Source { generation, first: first_sequence, last: None, head: None, tail: None, retired: false, subject: None });
        Ok(())
    }
    /// Bind the native activation once; an NPC's None is distinct from unbound.
    pub(super) fn bind_subject(&mut self, activation: [u8; 16], generation: u64, subject: Option<i64>) -> Result<(), Refusal> {
        let source = self.sources.get_mut(&activation).ok_or(Refusal::Invalid)?;
        if source.generation != generation || source.retired { return Err(Refusal::Invalid); }
        match source.subject {
            Some(previous) if previous != subject => Err(Refusal::Invalid),
            Some(_) => Ok(()),
            None => { source.subject = Some(subject); Ok(()) }
        }
    }

    pub(super) fn matches_subject(&self, activation: [u8; 16], subject: Option<i64>) -> bool {
        self.sources.get(&activation).is_some_and(|source| source.subject == Some(subject))
    }

    pub(super) fn subjects(&self) -> impl Iterator<Item = i64> + '_ {
        self.sources.values().filter_map(|source| source.subject.flatten())
    }

    pub(super) fn retire(&mut self, activation: [u8; 16], generation: u64) -> Result<(), Refusal> {
        let source = self.sources.get_mut(&activation).ok_or(Refusal::Invalid)?;
        if source.generation != generation || source.retired { return Err(Refusal::Invalid); }
        if source.head.is_some() {
            source.retired = true;
        } else {
            self.sources.remove(&activation);
        }
        Ok(())
    }

    /// Reserve strictly increasing native launch sequences, including misses and
    /// cancellations. Unreserved numeric gaps can never arrive later.
    pub(super) fn reserve(&mut self, activation: [u8; 16], generation: u64, sequence: u64) -> Result<(), Refusal> {
        let source = self.sources.get(&activation).ok_or(Refusal::Invalid)?;
        if source.generation != generation || source.retired { return Err(Refusal::Invalid); }
        if sequence < source.first || source.last.is_some_and(|last| sequence <= last) { return Err(Refusal::Replay); }
        if self.attempts.len() == self.capacity { return Err(Refusal::Full); }
        let node = Node { sequence, next: None, state: State::Reserved };
        let index = if let Some(index) = self.free.pop() {
            self.nodes[index] = Some(node);
            index
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        };
        let source = self.sources.get_mut(&activation).expect("registered source");
        if let Some(tail) = source.tail {
            self.nodes[tail].as_mut().expect("live tail").next = Some(index);
        } else {
            source.head = Some(index);
        }
        source.tail = Some(index);
        source.last = Some(sequence);
        self.attempts.insert((activation, sequence), index);
        Ok(())
    }

    /// Claim a previously reserved launch once, immediately before impact work.
    pub(super) fn claim(&mut self, activation: [u8; 16], generation: u64, sequence: u64) -> Result<(), Refusal> {
        let source = self.sources.get(&activation).ok_or(Refusal::Invalid)?;
        if source.generation != generation { return Err(Refusal::Invalid); }
        let index = *self.attempts.get(&(activation, sequence)).ok_or(Refusal::Replay)?;
        let node = self.nodes[index].as_mut().expect("indexed attempt");
        if node.state != State::Reserved { return Err(Refusal::Replay); }
        node.state = State::Claimed;
        Ok(())
    }

    /// Every prepared attempt is consumed, including rollback, miss and cancel.
    pub(super) fn finish(&mut self, activation: [u8; 16], generation: u64, sequence: u64, _accepted: bool) -> bool {
        let Some(source) = self.sources.get_mut(&activation) else { return false; };
        if source.generation != generation { return false; }
        let Some(&index) = self.attempts.get(&(activation, sequence)) else { return false; };
        self.nodes[index].as_mut().expect("indexed attempt").state = State::Completed;
        while let Some(head) = source.head {
            let node = self.nodes[head].as_ref().expect("live head");
            if node.state != State::Completed { break; }
            let sequence = node.sequence;
            source.head = node.next;
            if source.head.is_none() { source.tail = None; }
            self.attempts.remove(&(activation, sequence));
            self.nodes[head] = None;
            self.free.push(head);
        }
        if source.retired && source.head.is_none() {
            self.sources.remove(&activation);
            return true;
        }
        false
    }
}

#[cfg(test)] mod tests;
