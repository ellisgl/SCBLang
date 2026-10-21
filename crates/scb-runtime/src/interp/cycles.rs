//! The cycle collector (SPEC "Runtime": refcounting with a cycle collector;
//! "Destructors": cycles).
//!
//! Refcounting frees everything except reference cycles. Value types cannot form
//! one (copy-on-write never makes a value contain itself), so every cycle passes
//! through a class instance (directly, or through a closure stored in one). The
//! isolate keeps a weak reference to every instance; a collection runs trial
//! deletion over the containers reachable from them:
//!
//! 1. Gather every container (object, list, map, set, tuple, struct, enum,
//!    function) reachable from a live instance, holding one handle to each.
//! 2. Each container's count of outside references is its refcount, minus that
//!    handle, minus the references from containers in the set.
//! 3. A container with outside references (a local, a static, an argument, a value
//!    the interpreter holds) is live, and so is everything it reaches. The objects
//!    left over are garbage: they are only referenced by each other.
//!
//! References the collector cannot see (temporaries, queued destructors) count as
//! outside references, so a mistake can only make it collect less, never free a
//! live value. Garbage objects' destructors all run first (in unspecified order);
//! as a destructor may make its object reachable again, the analysis is repeated,
//! and then the fields of the objects still unreachable are cleared, which breaks
//! the cycles and frees them. In the repeated analysis an object whose destructor
//! has not run yet (one a destructor created) counts as live, with everything it
//! reaches: its destructor may still read them, so they wait for the next
//! collection.
//!
//! A collection runs at a statement boundary once the objects created since the
//! previous one outnumber the live values that collection visited (at least
//! [`MIN_THRESHOLD`]), so its cost per allocation is constant; and at program end.
//! Statements run by the destructors it calls do not start another. Dead entries
//! are pruned from the registry as it grows, independently: a `Weak` keeps its
//! object's allocation (not its fields) until it is dropped.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use super::{Interp, R};
use crate::value::{FuncValue, ObjectValue, Value};

/// Objects created before the first collection is considered.
pub(crate) const MIN_THRESHOLD: usize = 10_000;

impl Interp<'_> {
    /// Registers a new class instance with the collector.
    pub(crate) fn track(&mut self, v: &Value) {
        if let Value::Object(o) = v {
            self.iso.gc_created += 1;
            self.register(Rc::downgrade(o));
        }
    }

    /// Adds an object to the registry, pruning dead entries when it has doubled.
    pub(crate) fn register(&mut self, w: Weak<RefCell<ObjectValue>>) {
        let objects = &mut self.iso.objects;
        if objects.len() >= self.iso.gc_prune_at {
            objects.retain(|w| w.strong_count() > 0);
            self.iso.gc_prune_at = (objects.len() * 2).max(MIN_THRESHOLD);
        }
        objects.push(w);
    }

    /// Whether enough objects were created since the last collection.
    pub(crate) fn collection_due(&self) -> bool {
        self.iso.gc_created >= self.iso.gc_threshold && !self.iso.gc_running
    }

    /// Finds and frees unreachable cycles; runs their destructors first.
    pub(crate) fn collect_cycles(&mut self) -> R<()> {
        self.iso.gc_running = true;
        let r = self.collect();
        self.iso.gc_running = false;
        r
    }

    fn collect(&mut self) -> R<()> {
        let (garbage, mut work) = self.find_garbage(false);
        if !garbage.is_empty() {
            for o in &garbage {
                let decl = o.borrow().decl;
                // Each destructor runs once: the handle is taken.
                let due = o.borrow_mut().destructor.take().is_some();
                if due {
                    self.call_destructor(decl, Value::Object(o.clone()))?;
                }
            }
            drop(garbage);
            // Destructors may have made some objects reachable again, or created
            // new garbage whose destructors are still due.
            let (garbage, live_work) = self.find_garbage(true);
            work = live_work;
            let mut released = Vec::new();
            for o in &garbage {
                let mut o = o.borrow_mut();
                released.push((std::mem::take(&mut o.fields), o.native.take()));
            }
            // Dropping the fields may queue destructors of objects that hung off
            // the cycles; they run at once.
            drop(released);
            drop(garbage);
            self.run_destructors()?;
        }
        let objects = &mut self.iso.objects;
        objects.retain(|w| w.strong_count() > 0);
        self.iso.gc_prune_at = (objects.len() * 2).max(MIN_THRESHOLD);
        self.iso.gc_created = 0;
        self.iso.gc_threshold = work.max(MIN_THRESHOLD);
        Ok(())
    }

    /// The live objects that are only reachable from each other, and the number of
    /// values visited in the containers that stay live. With `pending_live`,
    /// objects whose destructor is still due count as live.
    fn find_garbage(&mut self, pending_live: bool) -> (Vec<Rc<RefCell<ObjectValue>>>, usize) {
        // 1. Gather: one handle per container, objects first. Indexes are `u32` and
        // the edges one flat array (node `k`'s are `targets[starts[k]..starts[k + 1]]`):
        // the bookkeeping is a few bytes per container.
        let mut nodes: Vec<Value> = Vec::new();
        let mut index: HashMap<usize, u32> = HashMap::new();
        for w in &self.iso.objects {
            if let Some(o) = w.upgrade() {
                let v = Value::Object(o);
                if let Some(key) = key_of(&v) {
                    index.entry(key).or_insert_with(|| {
                        nodes.push(v);
                        (nodes.len() - 1) as u32
                    });
                }
            }
        }
        let mut starts: Vec<u32> = vec![0];
        let mut targets: Vec<u32> = Vec::new();
        // Containers whose contents could not be read count as live.
        let mut opaque: Vec<bool> = Vec::new();
        // Per container: 1 + the values in it.
        let mut visits: Vec<u32> = Vec::new();
        let mut i = 0;
        while i < nodes.len() {
            let node = nodes[i].clone();
            let mut n: u32 = 1;
            let readable = children(&node, &mut |child| {
                n = n.saturating_add(1);
                if let Some(key) = key_of(child) {
                    let j = *index.entry(key).or_insert_with(|| {
                        nodes.push(child.clone());
                        (nodes.len() - 1) as u32
                    });
                    targets.push(j);
                }
            });
            starts.push(targets.len() as u32);
            opaque.push(!readable);
            visits.push(n);
            i += 1;
        }
        drop(index);
        let edges = |k: usize| &targets[starts[k] as usize..starts[k + 1] as usize];
        // 2. Outside references: refcount − our handle − references from the set.
        let mut outside: Vec<isize> = nodes.iter().map(|v| strong_count(v) as isize - 1).collect();
        for &j in &targets {
            outside[j as usize] -= 1;
        }
        // 3. Everything reachable from a node with outside references is live.
        let mut live: Vec<bool> = outside
            .iter()
            .zip(&opaque)
            .zip(&nodes)
            .map(|((&n, &o), v)| n > 0 || o || (pending_live && destructor_due(v)))
            .collect();
        drop(outside);
        let mut stack: Vec<usize> = (0..nodes.len()).filter(|&k| live[k]).collect();
        while let Some(k) = stack.pop() {
            for &j in edges(k) {
                if !live[j as usize] {
                    live[j as usize] = true;
                    stack.push(j as usize);
                }
            }
        }
        let work = (0..nodes.len())
            .filter(|&k| live[k])
            .map(|k| visits[k] as usize)
            .sum();
        let garbage = nodes
            .into_iter()
            .zip(live)
            .filter_map(|(v, l)| match v {
                Value::Object(o) if !l => Some(o),
                _ => None,
            })
            .collect();
        (garbage, work)
    }
}

/// A container's identity (its allocation), or `None` for values that cannot
/// reference an object (primitives, strings, bytes, ranges).
fn key_of(v: &Value) -> Option<usize> {
    Some(match v {
        Value::List(x) | Value::Tuple(x) => Rc::as_ptr(x) as *const () as usize,
        Value::Map(x) | Value::Set(x) => Rc::as_ptr(x) as *const () as usize,
        Value::Struct(x) => Rc::as_ptr(x) as *const () as usize,
        Value::Enum(x) => Rc::as_ptr(x) as *const () as usize,
        Value::Object(x) => Rc::as_ptr(x) as *const () as usize,
        Value::Func(x) => Rc::as_ptr(x) as *const () as usize,
        _ => return None,
    })
}

/// An object whose destructor has not run yet (readable: opaque objects are live
/// anyway).
fn destructor_due(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.try_borrow().is_ok_and(|o| o.destructor.is_some()),
        _ => false,
    }
}

fn strong_count(v: &Value) -> usize {
    match v {
        Value::List(x) | Value::Tuple(x) => Rc::strong_count(x),
        Value::Map(x) | Value::Set(x) => Rc::strong_count(x),
        Value::Struct(x) => Rc::strong_count(x),
        Value::Enum(x) => Rc::strong_count(x),
        Value::Object(x) => Rc::strong_count(x),
        Value::Func(x) => Rc::strong_count(x),
        _ => 0,
    }
}

/// Calls `f` on each value directly inside a container. `false` if the contents
/// could not be read (an object borrowed at the moment).
fn children(v: &Value, f: &mut dyn FnMut(&Value)) -> bool {
    match v {
        Value::List(x) | Value::Tuple(x) => x.iter().for_each(f),
        Value::Map(m) | Value::Set(m) => {
            for (k, v) in m.iter() {
                f(k);
                f(v);
            }
        }
        Value::Struct(s) => s.fields.iter().for_each(f),
        Value::Enum(e) => e.payload.iter().for_each(f),
        Value::Object(o) => match o.try_borrow() {
            Ok(o) => {
                o.fields.iter().for_each(&mut *f);
                if let Some(crate::value::NativeState::Throwable(t)) = o.native.as_deref()
                    && let Some(p) = &t.previous
                {
                    f(p);
                }
            }
            Err(_) => return false,
        },
        Value::Func(fv) => match &**fv {
            FuncValue::Closure { captures, .. } => captures.iter().for_each(|(_, v)| f(v)),
            FuncValue::Method { receiver, .. } => f(receiver),
            FuncValue::Builtin {
                receiver: Some(r), ..
            } => f(r),
            FuncValue::Builtin { receiver: None, .. } | FuncValue::Func(_) => {}
        },
        _ => {}
    }
    true
}
