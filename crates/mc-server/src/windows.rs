//! The windows a player has open: the inventory (always, window 0, with its
//! 2×2 crafting grid), a container (chest, barrel) or a crafting table.
//!
//! Window slots, as the protocol numbers them: the inventory is 0 result,
//! 1–4 grid, 5–8 armour, 9–35 main, 36–44 hotbar, 45 off hand. Any other
//! window has its own slots first, then the main inventory (27) and the
//! hotbar (9).

use mc_protocol::Position;
use mc_protocol::packets::play::{self, HashedStack, ItemStack};
use mc_protocol::text;

use super::{PacketWriter, change_hands};
use crate::containers;
use crate::context::{Event, Pose, ServerContext};
use crate::menus::{Machine, Menu};

enum Kind {
    /// One container, or a large chest made of two (window slots in order).
    Container { parts: Vec<(Position, usize)> },
    /// Crafting table: 0 result, 1–9 grid (not shared, not stored).
    Crafting { grid: Vec<Option<ItemStack>> },
    /// A menu with slots of its own (beacon, anvil...).
    Machine(Box<Machine>),
}

impl Kind {
    /// The part holding window slot `slot`, and the slot in it.
    fn part_of(&self, slot: usize) -> Option<(Position, usize)> {
        match self {
            Kind::Container { parts } => open_part(parts, slot),
            Kind::Crafting { .. } | Kind::Machine(_) => None,
        }
    }

    /// Where the part at `position` starts in the window.
    fn offset_of(&self, position: Position) -> Option<usize> {
        if let Kind::Machine(m) = self {
            return (m.at == position).then_some(0);
        }
        let Kind::Container { parts } = self else {
            return None;
        };
        let mut start = 0;
        for &(p, n) in parts {
            if p == position {
                return Some(start);
            }
            start += n;
        }
        None
    }

    fn size(&self) -> usize {
        match self {
            Kind::Container { parts } => parts.iter().map(|(_, n)| n).sum(),
            Kind::Crafting { .. } => 10,
            Kind::Machine(m) => m.slots.len(),
        }
    }
}

struct Open {
    id: i32,
    kind: Kind,
}

/// The player a window call is about.
#[derive(Clone, Copy)]
pub struct Who<'a> {
    pub ctx: &'a ServerContext,
    pub uuid: u128,
    pub entity_id: i32,
    pub pose: Pose,
}

/// One player's windows.
pub struct Windows {
    open: Option<Open>,
    next_id: i32,
    pub state_id: i32,
}

/// What the player's click was about.
pub struct Click {
    pub window: i32,
    pub slot: i16,
    pub button: i8,
    pub mode: i32,
    pub changed: Vec<(i16, Option<HashedStack>)>,
    pub carried: Option<HashedStack>,
}

impl Windows {
    pub fn new() -> Self {
        Self {
            open: None,
            next_id: 0,
            state_id: 1,
        }
    }

    fn set_slot(
        &mut self,
        writer: &mut PacketWriter,
        window: i32,
        slot: usize,
        item: Option<ItemStack>,
    ) -> anyhow::Result<()> {
        self.state_id += 1;
        writer.queue(&play::ContainerSetSlot {
            window,
            state_id: self.state_id,
            slot: slot as i16,
            item,
        })
    }

    fn show(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        menu: &str,
        title: &str,
        own: Vec<Option<ItemStack>>,
        kind: Kind,
    ) -> anyhow::Result<()> {
        let Who { ctx, uuid, .. } = who;
        self.next_id = self.next_id % 100 + 1;
        let menu = ctx.data.builtin.id("minecraft:menu", menu).unwrap_or(0);
        writer.queue(&play::OpenScreen {
            window: self.next_id,
            menu,
            title: text::plain(title),
        })?;
        let hands = ctx
            .inventories
            .lock()
            .expect("inventories lock")
            .get(&uuid)
            .cloned()
            .unwrap_or_default();
        let mut all = own;
        all.extend(hands.slots[9..45].iter().cloned());
        self.state_id += 1;
        writer.queue(&play::ContainerSetContent {
            window: self.next_id,
            state_id: self.state_id,
            slots: all,
            carried: hands.carried.clone(),
        })?;
        self.open = Some(Open {
            id: self.next_id,
            kind,
        });
        Ok(())
    }

    pub fn open_container(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        position: Position,
        kind: containers::Kind,
    ) -> anyhow::Result<()> {
        let Who { ctx, uuid, .. } = who;
        self.close_current(writer, who)?;
        // Two chest halves open together as a large chest, the right half
        // first.
        let state = ctx.world.block(position, &ctx.data.blocks).unwrap_or(0);
        let partner = ctx.shapes.chest_partner(state).and_then(|d| {
            let (dx, dy, dz) = d.offset();
            let other = Position::new(position.x + dx, position.y + dy, position.z + dz);
            let s = ctx.world.block(other, &ctx.data.blocks)?;
            (ctx.data.blocks.block_name(s) == ctx.data.blocks.block_name(state)
                && ctx.shapes.chest_partner(s) == Some(d.opposite()))
            .then_some(other)
        });
        let (parts, menu, title) = match partner {
            Some(other) => {
                let right = ctx.data.blocks.property(state, "type") == Some("right");
                let (first, second) = if right {
                    (position, other)
                } else {
                    (other, position)
                };
                (
                    vec![(first, kind.slots), (second, kind.slots)],
                    "minecraft:generic_9x6",
                    "Large Chest",
                )
            }
            None => (vec![(position, kind.slots)], kind.menu, kind.title),
        };
        let mut slots = Vec::new();
        for &(p, _) in &parts {
            slots.extend(containers::open(ctx, uuid, p, kind));
        }
        self.show(writer, who, menu, title, slots, Kind::Container { parts })?;
        // A furnace window shows its fire and progress.
        if kind.slots == 3 {
            let values = crate::furnaces::progress(ctx, position).values();
            self.container_data(writer, position, &values)?;
        }
        Ok(())
    }

    /// Opens the menu of the block at `at`.
    pub fn open_menu(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        at: Position,
        menu: Menu,
    ) -> anyhow::Result<()> {
        self.close_current(writer, who)?;
        let machine = Machine::new(who.ctx, menu, at);
        let values = machine.data.clone();
        let slots = machine.slots.clone();
        self.show(
            writer,
            who,
            menu.menu(),
            menu.title(),
            slots,
            Kind::Machine(Box::new(machine)),
        )?;
        self.send_values(writer, &values)
    }

    /// Sends the window's numbers.
    fn send_values(&mut self, writer: &mut PacketWriter, values: &[i16]) -> anyhow::Result<()> {
        let Some(id) = self.open.as_ref().map(|o| o.id) else {
            return Ok(());
        };
        for (property, &value) in values.iter().enumerate() {
            writer.queue(&play::ContainerSetData {
                window: id,
                property: property as i16,
                value,
            })?;
        }
        Ok(())
    }

    pub fn open_crafting(&mut self, writer: &mut PacketWriter, who: Who) -> anyhow::Result<()> {
        self.close_current(writer, who)?;
        let grid = vec![None; 10];
        self.show(
            writer,
            who,
            "minecraft:crafting",
            "Crafting",
            grid.clone(),
            Kind::Crafting { grid },
        )
    }

    /// A click in window 0 (the inventory) or the open window.
    pub fn click(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        click: Click,
    ) -> anyhow::Result<()> {
        let Who {
            ctx,
            uuid,
            entity_id,
            pose,
        } = who;
        if click.window == 0 {
            let mut thrown = Vec::new();
            let mut result = None;
            change_hands(ctx, uuid, entity_id, |hands| {
                thrown = hands.click(
                    click.slot,
                    click.button,
                    click.mode,
                    &click.changed,
                    click.carried.clone(),
                    &[],
                );
                // The 2×2 grid's result.
                let made = ctx.recipes.craft(&hands.slots[1..5], 2);
                if made != hands.slots[0] {
                    hands.slots[0] = made.clone();
                    result = Some(made);
                }
            });
            for stack in thrown {
                crate::tick::throw_item(ctx, pose, stack);
            }
            if let Some(made) = result {
                self.set_slot(writer, 0, 0, made)?;
            }
            return Ok(());
        }
        let Some(open) = self.open.as_mut().filter(|o| o.id == click.window) else {
            return Ok(());
        };
        let size = open.kind.size();
        let to_player = |s: usize| {
            if s < size + 27 {
                9 + s - size
            } else {
                36 + s - size - 27
            }
        };
        let (mut mine, mut theirs) = (Vec::new(), Vec::new());
        for (s, stack) in &click.changed {
            match usize::try_from(*s) {
                Ok(s) if s < size => theirs.push((s, stack.clone())),
                Ok(s) if s < size + 36 => mine.push((to_player(s) as i16, stack.clone())),
                _ => {}
            }
        }
        let player_slot = match usize::try_from(click.slot) {
            Ok(s) if (size..size + 36).contains(&s) => to_player(s) as i16,
            Ok(_) => -1,
            Err(_) => click.slot,
        };
        // Q over one of the window's own slots throws from there.
        if click.mode == 4
            && let Ok(s) = usize::try_from(click.slot)
            && s < size
        {
            let before = match &open.kind {
                Kind::Container { .. } => open.kind.part_of(s).and_then(|(p, local)| {
                    ctx.containers
                        .lock()
                        .expect("containers lock")
                        .get(&(p.x, p.y, p.z))
                        .and_then(|o| o.slots.get(local).cloned().flatten())
                }),
                Kind::Crafting { grid } => grid[s].clone(),
                Kind::Machine(m) => m.slots.get(s).cloned().flatten(),
            };
            if let Some(stack) = before {
                let count = if click.button == 1 { stack.count } else { 1 };
                crate::tick::throw_item(ctx, pose, ItemStack { count, ..stack });
            }
        }
        // What both sides held before, for the stacks' damage.
        let window_stacks: Vec<ItemStack> = match &open.kind {
            Kind::Container { parts } => {
                let all = ctx.containers.lock().expect("containers lock");
                parts
                    .iter()
                    .filter_map(|(p, _)| all.get(&(p.x, p.y, p.z)))
                    .flat_map(|o| o.slots.iter().flatten())
                    .filter(|s| s.has_changes())
                    .cloned()
                    .collect()
            }
            Kind::Crafting { grid } => grid
                .iter()
                .flatten()
                .filter(|s| s.has_changes())
                .cloned()
                .collect(),
            Kind::Machine(m) => m
                .slots
                .iter()
                .flatten()
                .filter(|s| s.has_changes())
                .cloned()
                .collect(),
        };
        let mut hand_stacks = Vec::new();
        let mut thrown = Vec::new();
        change_hands(ctx, uuid, entity_id, |hands| {
            hand_stacks = hands.changed_stacks();
            thrown = hands.click(
                player_slot,
                click.button,
                click.mode,
                &mine,
                click.carried.clone(),
                &window_stacks,
            );
        });
        for stack in thrown {
            crate::tick::throw_item(ctx, pose, stack);
        }
        let id = open.id;
        match &mut open.kind {
            Kind::Container { parts } => {
                for &(p, _) in parts.iter() {
                    let local: Vec<(usize, Option<HashedStack>)> = theirs
                        .iter()
                        .filter_map(|(s, stack)| {
                            let (q, slot) = open_part(parts, *s)?;
                            (q == p).then(|| (slot, stack.clone()))
                        })
                        .collect();
                    if local.is_empty() {
                        continue;
                    }
                    let (viewers, slots) = containers::change(ctx, uuid, p, &local, &hand_stacks);
                    if !viewers.is_empty() {
                        let _ = ctx.events.send(Event::ContainerSlots {
                            position: p,
                            viewers,
                            slots,
                        });
                    }
                }
            }
            Kind::Crafting { grid } => {
                for (s, stack) in theirs {
                    let known: Vec<&ItemStack> = hand_stacks.iter().chain(&window_stacks).collect();
                    grid[s] = stack.map(|h| h.resolve(known.iter().copied()));
                }
                let made = ctx.recipes.craft(&grid[1..10], 3);
                if made != grid[0] {
                    grid[0] = made.clone();
                    self.set_slot(writer, id, 0, made)?;
                }
            }
            Kind::Machine(m) => {
                let known: Vec<&ItemStack> = hand_stacks.iter().chain(&window_stacks).collect();
                let result = m.result_slot();
                let result_before = result.and_then(|r| m.slots[r].clone());
                for (s, stack) in theirs {
                    m.slots[s] = stack.map(|h| h.resolve(known.iter().copied()));
                }
                let taken = result.is_some_and(|r| result_before.is_some() && m.slots[r].is_none());
                let shown = m.slots.clone();
                m.settle(ctx, uuid, taken);
                self.sync_machine(writer, who, shown)?;
            }
        }
        Ok(())
    }

    /// Sends what a menu changed on its own since the client's view `shown`
    /// (results, numbers).
    fn sync_machine(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        shown: Vec<Option<ItemStack>>,
    ) -> anyhow::Result<()> {
        let Some(open) = self.open.as_mut() else {
            return Ok(());
        };
        let id = open.id;
        let Kind::Machine(m) = &mut open.kind else {
            return Ok(());
        };
        let changes: Vec<(usize, Option<ItemStack>)> = m
            .slots
            .iter()
            .enumerate()
            .filter(|(i, s)| **s != shown[*i])
            .map(|(i, s)| (i, s.clone()))
            .collect();
        let values = m.values(who.ctx);
        let changed_values = values != m.data;
        m.data.clone_from(&values);
        for (slot, item) in changes {
            self.set_slot(writer, id, slot, item)?;
        }
        if changed_values {
            self.send_values(writer, &values)?;
        }
        Ok(())
    }

    /// A button of the open window was pressed.
    pub fn button(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        window: i32,
        button: i32,
    ) -> anyhow::Result<()> {
        let Some(open) = self.open.as_mut().filter(|o| o.id == window) else {
            return Ok(());
        };
        let Kind::Machine(m) = &mut open.kind else {
            return Ok(());
        };
        let shown = m.slots.clone();
        m.button(who.ctx, who.uuid, button);
        self.sync_machine(writer, who, shown)
    }

    /// The client closed a window.
    pub fn close(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        window: i32,
    ) -> anyhow::Result<()> {
        let Who { ctx, uuid, .. } = who;
        if window == 0 {
            // The 2×2 grid empties back into the inventory.
            let grid: Vec<ItemStack> = {
                let mut inventories = ctx.inventories.lock().expect("inventories lock");
                match inventories.get_mut(&uuid) {
                    Some(hands) => {
                        hands.slots[0] = None;
                        (1..5).filter_map(|i| hands.slots[i].take()).collect()
                    }
                    None => Vec::new(),
                }
            };
            self.give_back(writer, who, grid)?;
            return self.drop_cursor(who);
        }
        if self.open.as_ref().is_some_and(|o| o.id == window) {
            self.close_current(writer, who)?;
        }
        Ok(())
    }

    fn drop_cursor(&mut self, who: Who) -> anyhow::Result<()> {
        let Who {
            ctx,
            uuid,
            entity_id,
            pose,
        } = who;
        let mut thrown = None;
        change_hands(ctx, uuid, entity_id, |hands| thrown = hands.carried.take());
        if let Some(stack) = thrown {
            crate::tick::throw_item(ctx, pose, stack);
        }
        Ok(())
    }

    /// Stacks go back into the inventory; what does not fit is thrown.
    fn give_back(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        stacks: Vec<ItemStack>,
    ) -> anyhow::Result<()> {
        let Who {
            ctx,
            uuid,
            entity_id,
            pose,
        } = who;
        for stack in stacks {
            let max = ctx.items.max_stack(stack.item);
            let mut result = (0, Vec::new());
            change_hands(ctx, uuid, entity_id, |hands| {
                result = hands.insert(stack.item, stack.count, max)
            });
            let (taken, changed) = result;
            let slots: Vec<(usize, Option<ItemStack>)> = {
                let inventories = ctx.inventories.lock().expect("inventories lock");
                let hands = inventories.get(&uuid);
                changed
                    .into_iter()
                    .map(|s| (s, hands.and_then(|h| h.slots[s].clone())))
                    .collect()
            };
            for (slot, item) in slots {
                self.set_slot(writer, 0, slot, item)?;
            }
            if taken < stack.count {
                crate::tick::throw_item(
                    ctx,
                    pose,
                    ItemStack {
                        count: stack.count - taken,
                        ..stack
                    },
                );
            }
        }
        Ok(())
    }

    fn close_current(&mut self, writer: &mut PacketWriter, who: Who) -> anyhow::Result<()> {
        let Who { ctx, uuid, .. } = who;
        let Some(open) = self.open.take() else {
            return Ok(());
        };
        match open.kind {
            Kind::Container { parts } => {
                for (p, _) in parts {
                    containers::close(ctx, uuid, p);
                }
            }
            Kind::Crafting { grid } => {
                let left: Vec<ItemStack> = grid.into_iter().skip(1).flatten().collect();
                self.give_back(writer, who, left)?;
            }
            Kind::Machine(mut m) => {
                let left = m.leftovers();
                self.give_back(writer, who, left)?;
            }
        }
        self.drop_cursor(who)
    }

    /// Others changed the container this player has open.
    pub fn container_slots(
        &mut self,
        writer: &mut PacketWriter,
        position: Position,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) -> anyhow::Result<()> {
        let Some((id, start)) = self
            .open
            .as_ref()
            .and_then(|o| Some((o.id, o.kind.offset_of(position)?)))
        else {
            return Ok(());
        };
        for (slot, item) in slots {
            self.set_slot(writer, id, start + slot, item)?;
        }
        Ok(())
    }

    /// Numbers of the container at `position` changed (furnace progress).
    pub fn container_data(
        &mut self,
        writer: &mut PacketWriter,
        position: Position,
        values: &[i16],
    ) -> anyhow::Result<()> {
        let Some(id) = self
            .open
            .as_ref()
            .and_then(|o| o.kind.offset_of(position).map(|_| o.id))
        else {
            return Ok(());
        };
        for (property, &value) in values.iter().enumerate() {
            writer.queue(&play::ContainerSetData {
                window: id,
                property: property as i16,
                value,
            })?;
        }
        Ok(())
    }

    /// The beacon window's choice of effects arrived.
    pub fn set_beacon(
        &mut self,
        writer: &mut PacketWriter,
        who: Who,
        primary: Option<i32>,
        secondary: Option<i32>,
    ) -> anyhow::Result<()> {
        let Who { ctx, .. } = who;
        let Some(open) = self.open.as_mut() else {
            return Ok(());
        };
        let id = open.id;
        let Kind::Machine(m) = &mut open.kind else {
            return Ok(());
        };
        if m.menu != Menu::Beacon {
            return Ok(());
        }
        let name = |e: Option<i32>| {
            e.and_then(|e| {
                ctx.data
                    .builtin
                    .get("minecraft:mob_effect")?
                    .iter()
                    .find(|(_, id)| **id == e)
                    .map(|(name, _)| name.clone())
            })
        };
        let (primary, secondary) = (name(primary), name(secondary));
        let (levels, _, _) = crate::beacons::state(ctx, m.at);
        let paid = m.slots[0]
            .as_ref()
            .is_some_and(|s| crate::beacons::is_payment(ctx, s.item));
        let valid = primary
            .as_deref()
            .is_some_and(|p| crate::beacons::primary_allowed(levels, p))
            && secondary.as_deref().is_none_or(|s| {
                crate::beacons::secondary_allowed(levels, primary.as_deref().unwrap_or(""), s)
            });
        if !paid || !valid {
            return Ok(());
        }
        if let Some(stack) = &mut m.slots[0] {
            stack.count -= 1;
            if stack.count <= 0 {
                m.slots[0] = None;
            }
        }
        let left = m.slots[0].clone();
        crate::beacons::choose(ctx, m.at, primary.as_deref(), secondary.as_deref());
        self.set_slot(writer, id, 0, left)
    }

    /// The container block is gone.
    pub fn container_gone(
        &mut self,
        writer: &mut PacketWriter,
        position: Position,
    ) -> anyhow::Result<()> {
        if let Some(open) = &self.open
            && open.kind.offset_of(position).is_some()
        {
            writer.queue(&play::ContainerCloseClientbound { window: open.id })?;
            self.open = None;
        }
        Ok(())
    }

    /// A slot of the inventory changed on the server (pickups, placing).
    pub fn inventory_slot(
        &mut self,
        writer: &mut PacketWriter,
        slot: i16,
        item: Option<ItemStack>,
    ) -> anyhow::Result<()> {
        self.set_slot(writer, 0, slot as usize, item)
    }
}

/// The part of a container window that holds window slot `slot`.
fn open_part(parts: &[(Position, usize)], slot: usize) -> Option<(Position, usize)> {
    let mut start = 0;
    for &(p, n) in parts {
        if slot < start + n {
            return Some((p, slot - start));
        }
        start += n;
    }
    None
}
