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
//! that never installs a source is byte-for-byte unchanged.

use bevy::{
    ecs::{component::ComponentId, world::unsafe_world_cell::UnsafeWorldCell},
    prelude::*,
    ptr::Ptr,
};

use crate::shared::replicon_tick::RepliconTick;

/// Outcome of consulting a [`ConfirmedReplicationSource`] for one component on
/// one entity at the current `ServerTick`.
///
/// For a present component only the serialized *value* is redirected. Change
/// detection — whether to send at all this tick — stays on the live component's
/// change ticks (replicon's existing `is_changed`), which is what receivers'
/// rollback/resim relies on: a dynamic body's authoritative components are
/// rewritten every simulation step, so they replicate every tick regardless of
/// whether the *value* changed. The value that goes on the wire, though, is the
/// confirmed one.
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
/// no aliasing mutable access). Any [`Ptr`] returned in
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
/// The implementation must only *read* through the cell.
pub type ConfirmedAheadFn =
    for<'w> unsafe fn(UnsafeWorldCell<'w>, Entity, ComponentId, RepliconTick) -> ConfirmedAhead;

/// The functions a [`ConfirmedReplicationSource`] reads confirmed history with.
#[derive(Clone, Copy)]
pub(crate) struct ConfirmedHistoryFns {
    pub(crate) lookup: ConfirmedLookupFn,
    pub(crate) ahead: ConfirmedAheadFn,
}

/// Optional authority-side source of confirmed component values.
///
/// Unset by default: replication serializes live components (the lead-0 /
/// authoritative-server behaviour). Installing a [`ConfirmedLookupFn`] makes the
/// authority replicate `history[ServerTick]` while its live components run ahead
/// at the predicted present.
#[derive(Resource, Default)]
pub struct ConfirmedReplicationSource(Option<ConfirmedHistoryFns>);

impl ConfirmedReplicationSource {
    /// Install the lookups, replacing any previous ones.
    pub fn set(&mut self, lookup: ConfirmedLookupFn, ahead: ConfirmedAheadFn) {
        self.0 = Some(ConfirmedHistoryFns { lookup, ahead });
    }

    /// Remove the lookups, reverting to live-component serialization.
    pub fn clear(&mut self) {
        self.0 = None;
    }

    /// The installed lookups, if any.
    pub(crate) fn fns(&self) -> Option<ConfirmedHistoryFns> {
        self.0
    }
}
