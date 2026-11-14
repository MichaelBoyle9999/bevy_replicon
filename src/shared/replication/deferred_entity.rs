use core::{alloc::Layout, mem, num::NonZeroUsize, ptr::NonNull};

use bevy::{
    ecs::component::{ComponentId, Mutable},
    prelude::*,
    ptr::PtrMut,
};

/// Like [`EntityWorldMut`], but buffers all structural changes.
///
/// Components are deserialized one by one, and to avoid causing archetype moves
/// or triggering observers without all components being inserted, we buffer all
/// insertions and removals and apply them as a single removal bundle and a single
/// insertion bundle.
#[derive(Deref)]
pub struct DeferredEntity<'w> {
    #[deref]
    entity: EntityWorldMut<'w>,
    changes: &'w mut DeferredChanges,
}

impl<'w> DeferredEntity<'w> {
    /// Wraps entity with a differed buffer.
    pub fn new(entity: EntityWorldMut<'w>, changes: &'w mut DeferredChanges) -> Self {
        changes.clear();
        Self { entity, changes }
    }

    /// Like [`EntityWorldMut::insert`], but accepts only a single component insertion and buffers it.
    ///
    /// Calling this function multiple times for different components is equivalent to inserting a bundle with them.
    pub fn insert<C: Component>(&mut self, component: C) -> &mut Self {
        let component_id = self.register_component::<C>();
        // SAFETY: component ID belongs to this type.
        unsafe { self.changes.insertions.insert(component, component_id) };
        self
    }

    /// Like [`EntityWorldMut::remove`], but accepts only a single component removal and buffers it.
    ///
    /// Calling this function multiple times for different components is equivalent to removing a bundle with them.
    pub fn remove<C: Component>(&mut self) -> &mut Self {
        let component_id = self.register_component::<C>();
        self.changes.removals.push(component_id);
        self
    }

    /// Gets mutable access to the component of type `C` for the current entity.
    ///
    /// Returns `None` if the entity does not have a component of type `C`.
    #[inline]
    pub fn get_mut<C: Component<Mutability = Mutable>>(&mut self) -> Option<Mut<'_, C>> {
        self.entity.get_mut()
    }

    /// Gets mutable access to the entity's component of type `C`, else to its buffered
    /// insertion, buffering `f`'s value first if there is none.
    pub fn get_mut_or_insert_with<C: Component<Mutability = Mutable>>(
        &mut self,
        f: impl FnOnce() -> C,
    ) -> &mut C {
        let component_id = self.register_component::<C>();
        if self.entity.contains_id(component_id) {
            return self.entity.get_mut::<C>().unwrap().into_inner();
        }
        // SAFETY: component ID belongs to this type.
        unsafe {
            self.changes
                .insertions
                .get_mut_or_insert_with(component_id, f)
        }
    }

    fn register_component<C: Component>(&mut self) -> ComponentId {
        // SAFETY: no location update is needed because we only register the component ID.
        unsafe { self.world_mut().register_component::<C>() }
    }

    /// Returns this entity's world.
    ///
    /// # Safety
    ///
    /// Must only be used to make non-structural ECS changes,
    /// similar to [`DeferredWorld`](bevy::ecs::world::DeferredWorld).
    pub unsafe fn world_mut(&mut self) -> &mut World {
        unsafe { self.entity.world_mut() }
    }

    /// Flushes the world and applies all buffered changes.
    ///
    /// Needed to be called after processing each entity
    /// to spawn all allocated entities from mappings.
    pub fn flush(&mut self) {
        self.changes.apply(&mut self.entity);
    }
}

/// Buffered changes for [`DeferredEntity`].
#[derive(Default)]
pub struct DeferredChanges {
    removals: Vec<ComponentId>,
    insertions: DeferredInsertions,
}

impl DeferredChanges {
    fn apply(&mut self, entity: &mut EntityWorldMut) {
        if !self.removals.is_empty() {
            entity.remove_by_ids(&self.removals);
        }

        if !self.insertions.is_empty() {
            self.insertions.apply(entity);
        }

        self.clear();
    }

    fn clear(&mut self) {
        self.removals.clear();
        self.insertions.clear();
    }
}

/// Buffered insertions stored in type-erased way.
///
/// Each buffered component is dropped exactly once: moved into the entity by [`Self::apply`],
/// or dropped in place by [`Self::clear`] or on drop.
#[derive(Default)]
pub(crate) struct DeferredInsertions {
    ids: Vec<ComponentId>,
    components: Vec<BufferedComponent>,
    data: AlignedBuffer,
}

/// Where a buffered component sits in [`AlignedBuffer`], and how to drop it there.
struct BufferedComponent {
    offset: usize,
    drop: unsafe fn(NonNull<u8>),
}

/// # Safety
///
/// `ptr` should point to a valid `C` that is never used again.
unsafe fn drop_buffered<C>(ptr: NonNull<u8>) {
    // SAFETY: the caller guarantees that the pointer holds a `C` nothing else will use.
    unsafe { ptr.cast::<C>().drop_in_place() }
}

impl DeferredInsertions {
    /// Moves component data into a dynamically allocated buffer.
    ///
    /// # Safety
    ///
    /// Component ID should correspond to the passed type, otherwise [`Self::apply`] won't
    /// write the data correctly.
    unsafe fn insert<C: Component>(&mut self, component: C, component_id: ComponentId) {
        let offset = self.data.reserve(Layout::new::<C>());
        self.ids.push(component_id);
        self.components.push(BufferedComponent {
            offset,
            drop: drop_buffered::<C>,
        });

        // SAFETY: the reserved space is allocated and aligned for `C`.
        unsafe { self.data.at(offset).cast::<C>().write(component) };
    }

    /// Returns the buffered `C`, buffering `f`'s value first if there is none.
    ///
    /// # Safety
    ///
    /// Component ID should correspond to the passed type.
    unsafe fn get_mut_or_insert_with<C: Component>(
        &mut self,
        component_id: ComponentId,
        f: impl FnOnce() -> C,
    ) -> &mut C {
        let index = match self.ids.iter().position(|&id| id == component_id) {
            Some(index) => index,
            None => {
                // SAFETY: the caller guarantees that the ID belongs to `C`.
                unsafe { self.insert(f(), component_id) };
                self.ids.len() - 1
            }
        };

        // SAFETY: the data at this offset was written as `C`, the type of its ID.
        unsafe {
            self.data
                .at(self.components[index].offset)
                .cast::<C>()
                .as_mut()
        }
    }

    fn apply(&mut self, entity: &mut EntityWorldMut) {
        // Taken before moving, so a panic while inserting leaks the components instead of dropping them twice.
        let mut components = mem::take(&mut self.components);
        // SAFETY: iterator produces valid pointers for each component ID.
        unsafe {
            let iter = components
                .iter()
                .map(|component| PtrMut::new(self.data.at(component.offset)).promote());
            entity.insert_by_ids(&self.ids, iter);
        }
        components.clear();
        self.components = components;
    }

    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn clear(&mut self) {
        self.ids.clear();
        for component in self.components.drain(..) {
            // SAFETY: the component wasn't applied, and draining forgets it.
            unsafe { (component.drop)(self.data.at(component.offset)) };
        }
        self.data.clear();
    }
}

impl Drop for DeferredInsertions {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Bytes whose start is aligned for every layout reserved in them.
struct AlignedBuffer {
    ptr: NonNull<u8>,
    len: usize,
    capacity: Layout,
}

// SAFETY: the buffer holds only components, which are `Send` and `Sync`.
unsafe impl Send for AlignedBuffer {}
// SAFETY: as above.
unsafe impl Sync for AlignedBuffer {}

impl Default for AlignedBuffer {
    fn default() -> Self {
        Self {
            ptr: NonNull::dangling(),
            len: 0,
            capacity: Layout::new::<()>(),
        }
    }
}

impl AlignedBuffer {
    /// Reserves space for `layout` at an offset whose address is aligned for it, and returns the offset.
    fn reserve(&mut self, layout: Layout) -> usize {
        let offset = self.len.next_multiple_of(layout.align());
        let end = offset
            .checked_add(layout.size())
            .expect("buffered insertions should fit in the address space");
        if end > self.capacity.size() || layout.align() > self.capacity.align() {
            let size = end.max(self.capacity.size().saturating_mul(2));
            let align = layout.align().max(self.capacity.align());
            self.reallocate(
                Layout::from_size_align(size, align)
                    .expect("buffered insertions should fit in the address space"),
            );
        }
        self.len = end;
        offset
    }

    fn reallocate(&mut self, capacity: Layout) {
        let ptr = if capacity.size() == 0 {
            NonNull::without_provenance(
                NonZeroUsize::new(capacity.align()).expect("alignment should be non-zero"),
            )
        } else {
            // SAFETY: the layout has a non-zero size.
            let ptr = unsafe { alloc::alloc::alloc(capacity) };
            NonNull::new(ptr).unwrap_or_else(|| alloc::alloc::handle_alloc_error(capacity))
        };
        // SAFETY: both allocations hold at least `len` bytes and are distinct.
        unsafe { ptr.copy_from_nonoverlapping(self.ptr, self.len) };
        self.deallocate();
        self.ptr = ptr;
        self.capacity = capacity;
    }

    fn deallocate(&mut self) {
        if self.capacity.size() != 0 {
            // SAFETY: the pointer was allocated with this layout.
            unsafe { alloc::alloc::dealloc(self.ptr.as_ptr(), self.capacity) };
        }
    }

    /// Returns a pointer to the byte at `offset`.
    ///
    /// # Safety
    ///
    /// `offset` should be within the reserved bytes.
    unsafe fn at(&mut self, offset: usize) -> NonNull<u8> {
        // SAFETY: the caller guarantees that the offset is within the allocation.
        unsafe { self.ptr.byte_add(offset) }
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        self.deallocate();
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::any::Any;

    use super::*;

    #[test]
    fn buffering() {
        let mut world = World::new();
        let before_archetypes = world.archetypes().len();
        let mut changes = DeferredChanges::default();
        let mut entity = DeferredEntity::new(world.spawn_empty(), &mut changes);

        entity
            .insert(Unit)
            .insert(Trivial(1))
            .insert(WithVec(vec![2, 3]))
            .insert(WithBox(Box::new(Trivial(4))))
            .insert(WithArc(Arc::new(Trivial(5))));

        entity.flush();
        let after_archetypes = entity.world().archetypes().len();

        assert!(entity.get::<Unit>().is_some());
        assert_eq!(**entity.get::<Trivial>().unwrap(), 1);
        assert_eq!(**entity.get::<WithVec>().unwrap(), [2, 3]);

        let with_box = entity.get::<WithBox>().unwrap();
        assert_eq!(**with_box.downcast_ref::<Trivial>().unwrap(), 4);

        let with_arc = entity.get::<WithArc>().unwrap();
        assert_eq!(Arc::strong_count(with_arc), 1);
        assert_eq!(**with_arc.downcast_ref::<Trivial>().unwrap(), 5);

        assert_eq!(
            after_archetypes - before_archetypes,
            1,
            "insertions should batch into one archetype move"
        );

        entity
            .remove::<Unit>()
            .remove::<Trivial>()
            .remove::<WithVec>()
            .remove::<WithBox>()
            .remove::<WithArc>();

        entity.flush();

        assert!(!entity.contains::<Unit>());
        assert!(!entity.contains::<Trivial>());
        assert!(!entity.contains::<WithVec>());
        assert!(!entity.contains::<WithBox>());
        assert!(!entity.contains::<WithArc>());
        assert_eq!(
            entity.world().archetypes().len(),
            after_archetypes,
            "removals shouldn't create intermediate archetypes"
        );
    }

    #[test]
    fn get_mut_or_insert_with() {
        let mut world = World::new();
        let mut changes = DeferredChanges::default();
        let mut entity = DeferredEntity::new(world.spawn(Trivial(1)), &mut changes);

        entity.get_mut_or_insert_with(|| Trivial(0)).0 += 1;
        entity.insert(Unit).insert(WithBox(Box::new(Trivial(4))));
        entity.get_mut_or_insert_with(|| WithVec(vec![1])).0.push(2);
        entity
            .get_mut_or_insert_with(|| WithVec(Vec::new()))
            .0
            .push(3);
        assert!(
            !entity.contains::<WithVec>(),
            "a buffered component should wait for the flush"
        );

        entity.flush();

        assert_eq!(**entity.get::<Trivial>().unwrap(), 2);
        assert!(entity.contains::<Unit>());
        let with_box = entity.get::<WithBox>().unwrap();
        assert_eq!(**with_box.downcast_ref::<Trivial>().unwrap(), 4);
        assert_eq!(**entity.get::<WithVec>().unwrap(), [1, 2, 3]);
    }

    #[test]
    fn buffered_components_are_aligned() {
        let mut world = World::new();
        let mut changes = DeferredChanges::default();
        let mut entity = DeferredEntity::new(world.spawn_empty(), &mut changes);

        entity
            .insert(Byte(1))
            .insert(AlignedUnit)
            .insert(Align32(2))
            .insert(Bytes3([3; 3]))
            .insert(Align4096(4))
            .insert(Trivial(5));

        assert!(is_aligned(entity.get_mut_or_insert_with(|| Byte(0))));
        assert!(is_aligned(entity.get_mut_or_insert_with(|| AlignedUnit)));
        assert!(is_aligned(entity.get_mut_or_insert_with(|| Align32(0))));
        assert!(is_aligned(entity.get_mut_or_insert_with(|| Bytes3([0; 3]))));
        assert!(is_aligned(entity.get_mut_or_insert_with(|| Align4096(0))));
        assert!(is_aligned(entity.get_mut_or_insert_with(|| Trivial(0))));

        entity.flush();

        assert_eq!(*entity.get::<Byte>().unwrap(), Byte(1));
        assert!(entity.contains::<AlignedUnit>());
        assert_eq!(*entity.get::<Align32>().unwrap(), Align32(2));
        assert_eq!(*entity.get::<Bytes3>().unwrap(), Bytes3([3; 3]));
        assert_eq!(*entity.get::<Align4096>().unwrap(), Align4096(4));
        assert_eq!(**entity.get::<Trivial>().unwrap(), 5);
    }

    #[test]
    fn buffer_reused_after_flush_keeps_alignment() {
        let mut world = World::new();
        let mut changes = DeferredChanges::default();
        let mut first = DeferredEntity::new(world.spawn_empty(), &mut changes);
        first.insert(Align4096(1));
        first.flush();

        let mut second = DeferredEntity::new(world.spawn_empty(), &mut changes);
        second.insert(Byte(2));
        assert!(is_aligned(second.get_mut_or_insert_with(|| Align4096(3))));
        second.flush();

        assert_eq!(*second.get::<Byte>().unwrap(), Byte(2));
        assert_eq!(*second.get::<Align4096>().unwrap(), Align4096(3));
    }

    #[test]
    fn cleared_insertions_drop_once() {
        let counted = Arc::new(());
        let mut world = World::new();
        let mut changes = DeferredChanges::default();
        let mut entity = DeferredEntity::new(world.spawn_empty(), &mut changes);
        entity.insert(Counted(counted.clone()));
        entity.get_mut_or_insert_with(|| Counted(counted.clone()));
        entity
            .get_mut_or_insert_with(|| WithVec(Vec::new()))
            .0
            .push(1);
        assert_eq!(Arc::strong_count(&counted), 2);

        let mut next = DeferredEntity::new(world.spawn_empty(), &mut changes);
        assert_eq!(
            Arc::strong_count(&counted),
            1,
            "an unflushed insertion should drop when the buffer is reused"
        );

        next.flush();
        assert!(!next.contains::<Counted>());
        assert!(!next.contains::<WithVec>());
        assert_eq!(Arc::strong_count(&counted), 1);
    }

    #[test]
    fn dropped_insertions_drop_once() {
        let counted = Arc::new(());
        let mut world = World::new();
        let mut changes = DeferredChanges::default();
        DeferredEntity::new(world.spawn_empty(), &mut changes)
            .insert(Counted(counted.clone()))
            .insert(Trivial(1));
        assert_eq!(Arc::strong_count(&counted), 2);

        drop(changes);
        assert_eq!(
            Arc::strong_count(&counted),
            1,
            "an unflushed insertion should drop with the buffer"
        );
    }

    #[test]
    fn flushed_insertions_move_into_entity() {
        let counted = Arc::new(());
        let mut world = World::new();
        let mut changes = DeferredChanges::default();
        let mut entity = DeferredEntity::new(world.spawn_empty(), &mut changes);
        entity.insert(Counted(counted.clone()));
        entity.flush();
        let id = entity.id();
        assert_eq!(Arc::strong_count(&counted), 2);

        DeferredEntity::new(world.spawn_empty(), &mut changes).flush();
        drop(changes);
        assert_eq!(
            Arc::strong_count(&counted),
            2,
            "a flushed insertion belongs to the entity"
        );

        world.despawn(id);
        assert_eq!(Arc::strong_count(&counted), 1);
    }

    fn is_aligned<C>(component: &mut C) -> bool {
        (component as *mut C).is_aligned()
    }

    #[derive(Component, Debug, PartialEq)]
    struct Byte(u8);

    #[derive(Component, Debug, PartialEq)]
    struct Bytes3([u8; 3]);

    #[derive(Component)]
    #[repr(align(64))]
    struct AlignedUnit;

    #[derive(Component, Debug, PartialEq)]
    #[repr(align(32))]
    struct Align32(u8);

    #[derive(Component, Debug, PartialEq)]
    #[repr(align(4096))]
    struct Align4096(u8);

    #[derive(Component)]
    struct Unit;

    #[derive(Component)]
    struct Counted(#[expect(dead_code, reason = "held for its strong count")] Arc<()>);

    #[derive(Component, Deref)]
    struct Trivial(usize);

    #[derive(Component, Deref)]
    struct WithVec(Vec<u8>);

    #[derive(Component, Deref)]
    struct WithBox(Box<dyn Any + Send + Sync>);

    #[derive(Component, Deref)]
    struct WithArc(Arc<dyn Any + Send + Sync>);
}
