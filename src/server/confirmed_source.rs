//! Authority-side hook for serializing a *confirmed* (past-tick) component value
//! while the live component holds a predicted present.
//!
//! By default replicon serializes the live component pointer stamped with
//! [`ServerTick`](super::server_tick::ServerTick) — correct when the authority's
//! simulation *is* the confirmed tick (the hardcoded-lead / authoritative-server
//! model). When the authority runs its simulation ahead of the tick it
//! replicates (a listen-server host that predicts the shared present and only
//! asserts state for ticks whose inputs it has), the serialized value must come
//! from history at `ServerTick`, not the live component.
//!
//! This module adds a single optional indirection: [`ConfirmedReplicationSource`]
//! holds a [`ConfirmedLookupFn`] that, for one replicated component on one entity
//! at the current `ServerTick`, decides whether to serialize the live value, a
//! confirmed value from history, or to withhold the component (it does not exist
//! at that tick yet). Replicon stays agnostic to *where* the confirmed value
//! lives; a rollback/history crate installs the lookup. Serializing the live
//! value is the degenerate case where `history[ServerTick] == live`, so an app
//! that never installs a source is byte-for-byte unchanged. Its
//! [`ConfirmedPresentFn`] likewise decides from which tick an entity exists for
//! clients that haven't received it.

use bevy::{
    ecs::{component::ComponentId, world::unsafe_world_cell::UnsafeWorldCell},
    prelude::*,
    ptr::Ptr,
};

use crate::shared::replicon_tick::RepliconTick;

/// Outcome of consulting a [`ConfirmedReplicationSource`] for one component on
/// one entity at the current `ServerTick`.
///
/// For a [`Self::Confirmed`] component, change detection — whether to send it
/// to a client at all this tick — reads the confirmed history too, through
/// [`ConfirmedChangedFn`], instead of the live component's change ticks: the
/// live component runs ahead, so it changes at ticks the client has yet to
/// reach and stops changing while confirmed changes are still unsent.
///
/// A live removal is withheld until the lookup returns [`Self::Removed`], and
/// sent at that `ServerTick`. One that [`ConfirmedAheadFn`] no longer reports
/// ahead while the component is present live again is dropped.
pub enum ConfirmedLookup<'w> {
    /// Not history-sourced — replicon serializes the live component, as usual.
    Live,
    /// History-sourced: serialize this confirmed value instead of the live one.
    /// Must point to a value of the same type as the component's live storage
    /// (the same `fns` serialize it).
    Confirmed(Ptr<'w>),
    /// History-sourced, but there is no confirmed value at this tick yet — the
    /// entity/component did not exist at `ServerTick`. The component is withheld
    /// this frame; the live (unconfirmed present) value is never leaked.
    Unconfirmed,
    /// History-sourced, and the component was removed at or before `ServerTick`.
    /// A present component is withheld like [`Self::Unconfirmed`]; a live removal
    /// is sent.
    Removed,
}

/// Decides the confirmed value for `(entity, component)` at `tick`, reading from
/// the world through the cell.
///
/// # Safety
///
/// The implementation must only *read* through the cell (no structural changes,
/// no aliasing mutable access), and only the components the source declares
/// (see [`ConfirmedReplicationSource::set`]). Any [`Ptr`] returned in
/// [`ConfirmedLookup::Confirmed`] must point to a live value of the same type as
/// `component`'s storage, valid for the cell's lifetime.
pub type ConfirmedLookupFn = for<'w> unsafe fn(
    UnsafeWorldCell<'w>,
    Entity,
    ComponentId,
    RepliconTick,
) -> ConfirmedLookup<'w>;

/// What the confirmed history of one component on one entity records at the
/// ticks after a given tick.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ConfirmedAhead {
    /// The component is confirmed present at some later tick.
    pub present: bool,
    /// The component is confirmed removed at some later tick.
    pub removed: bool,
}

/// What the confirmed history of `component` on `entity` records after `tick`,
/// reading from the world through the cell.
///
/// Only asked for components the [`ConfirmedLookupFn`] reports as
/// history-sourced. A withheld removal with no removal ahead while the
/// component is present live again can no longer reach clients, since the
/// confirmed state never shows it, so it is dropped. While the component is
/// absent live, the history may not have recorded its removal yet, so a removal
/// stays withheld while the component is confirmed present ahead.
///
/// # Safety
///
/// The implementation must only *read* through the cell, and only the
/// components the source declares (see [`ConfirmedReplicationSource::set`]).
pub type ConfirmedAheadFn =
    for<'w> unsafe fn(UnsafeWorldCell<'w>, Entity, ComponentId, RepliconTick) -> ConfirmedAhead;

/// Whether the confirmed history of `component` on `entity` records a change,
/// a new value or a removal, at a tick after `after`, up to and including
/// `until`, reading from the world through the cell.
///
/// Only asked for components the [`ConfirmedLookupFn`] reports as
/// [`ConfirmedLookup::Confirmed`] at `until`, with `after` the last tick a
/// client acknowledged for the entity. A history that no longer reaches back
/// to `after` must report a change, since the client's value can no longer be
/// compared. A change is never looked for at or before `after`, so the history
/// up to a client's acknowledged tick must be final: rewriting it, such as by
/// resimulating from before [`ServerTick`](super::server_tick::ServerTick),
/// leaves that client with the old value.
///
/// # Safety
///
/// The implementation must only *read* through the cell, and only the
/// components the source declares (see [`ConfirmedReplicationSource::set`]).
pub type ConfirmedChangedFn =
    for<'w> unsafe fn(UnsafeWorldCell<'w>, Entity, ComponentId, RepliconTick, RepliconTick) -> bool;

/// Whether the confirmed history records `entity` as present at `tick`,
/// reading from the world through the cell.
///
/// An entity, and its [`Signature`](crate::shared::replication::signature::Signature)
/// mapping, are sent to a client that hasn't received it only from the first
/// [`ServerTick`](super::server_tick::ServerTick) at which it is present, so it
/// reaches the client in the update dated at its arrival, with that tick's
/// confirmed values. Asked only for entities a client hasn't received.
///
/// Nothing else naming the entity is withheld: a replicated entity reference to it
/// makes the client spawn an empty entity for it ahead of its arrival, and a server
/// message naming it can't be mapped, so its sender checks the entity's presence.
///
/// # Safety
///
/// The implementation must only *read* through the cell, and only the
/// components the source declares (see [`ConfirmedReplicationSource::set`]).
pub type ConfirmedPresentFn = for<'w> unsafe fn(UnsafeWorldCell<'w>, Entity, RepliconTick) -> bool;

/// The functions a [`ConfirmedReplicationSource`] reads confirmed history with.
#[derive(Clone, Copy)]
pub(crate) struct ConfirmedHistoryFns {
    pub(crate) lookup: ConfirmedLookupFn,
    pub(crate) ahead: ConfirmedAheadFn,
    pub(crate) changed: ConfirmedChangedFn,
    pub(crate) present: ConfirmedPresentFn,
}

/// Optional authority-side source of confirmed component values.
///
/// Unset by default: replication serializes live components (the lead-0 /
/// authoritative-server behaviour). Installing a [`ConfirmedLookupFn`] makes the
/// authority replicate `history[ServerTick]` while its live components run ahead
/// at the predicted present.
///
/// The components the installed functions read are part of the replication
/// systems' declared access, so the scheduler never runs a system writing them
/// concurrently with replication, and rejects one that writes them in a
/// replication system's own parameters. That access is fixed once the
/// replication systems initialize, so installing afterwards panics.
#[derive(Resource)]
pub struct ConfirmedReplicationSource {
    fns: Option<ConfirmedHistoryFns>,
    reads: Vec<ComponentId>,
    access_declared: bool,
}

impl ConfirmedReplicationSource {
    /// An unset source whose access is still undeclared.
    pub(crate) fn unset() -> Self {
        Self {
            fns: None,
            reads: Vec::new(),
            access_declared: false,
        }
    }

    /// Install the lookups, replacing any previous ones, with `reads` the
    /// components they read through the world cell.
    ///
    /// # Safety
    ///
    /// `reads` must hold, as ids of this world, every component the lookups
    /// read, since replication declares no other access for them.
    ///
    /// # Panics
    ///
    /// Panics once the replication systems have initialized, since their
    /// declared access no longer changes.
    pub unsafe fn set(
        &mut self,
        lookup: ConfirmedLookupFn,
        ahead: ConfirmedAheadFn,
        changed: ConfirmedChangedFn,
        present: ConfirmedPresentFn,
        reads: impl IntoIterator<Item = ComponentId>,
    ) {
        assert!(
            !self.access_declared,
            "a confirmed replication source must be installed before the replication systems initialize"
        );
        self.fns = Some(ConfirmedHistoryFns {
            lookup,
            ahead,
            changed,
            present,
        });
        self.reads = reads.into_iter().collect();
    }

    /// Remove the lookups, reverting to live-component serialization.
    pub fn clear(&mut self) {
        self.fns = None;
        self.reads.clear();
    }

    /// The installed lookups, if any.
    pub(crate) fn fns(&self) -> Option<ConfirmedHistoryFns> {
        self.fns
    }

    /// The components the installed lookups read, from here on fixed.
    pub(crate) fn declare_reads(&mut self) -> &[ComponentId] {
        self.access_declared = true;
        &self.reads
    }
}
