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

enum Kind {
    Container {
        position: Position,
        size: usize,
    },
    /// Crafting table: 0 result, 1–9 grid (not shared, not stored).
    Crafting {
        grid: Vec<Option<ItemStack>>,
    },
}

impl Kind {
    fn size(&self) -> usize {
        match self {
            Kind::Container { size, .. } => *size,
            Kind::Crafting { .. } => 10,
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
        let slots = containers::open(ctx, uuid, position, kind);
        self.show(
            writer,
            who,
            kind.menu,
            kind.title,
            slots,
            Kind::Container {
                position,
                size: kind.slots,
            },
        )
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
                Kind::Container { position, .. } => ctx
                    .containers
                    .lock()
                    .expect("containers lock")
                    .get(&(position.x, position.y, position.z))
                    .and_then(|o| o.slots.get(s).cloned().flatten()),
                Kind::Crafting { grid } => grid[s].clone(),
            };
            if let Some(stack) = before {
                let count = if click.button == 1 { stack.count } else { 1 };
                crate::tick::throw_item(ctx, pose, ItemStack { count, ..stack });
            }
        }
        let mut thrown = Vec::new();
        change_hands(ctx, uuid, entity_id, |hands| {
            thrown = hands.click(
                player_slot,
                click.button,
                click.mode,
                &mine,
                click.carried.clone(),
            );
        });
        for stack in thrown {
            crate::tick::throw_item(ctx, pose, stack);
        }
        let id = open.id;
        match &mut open.kind {
            Kind::Container { position, .. } => {
                if !theirs.is_empty() {
                    let (viewers, slots) = containers::change(ctx, uuid, *position, &theirs);
                    if !viewers.is_empty() {
                        let _ = ctx.events.send(Event::ContainerSlots {
                            position: *position,
                            viewers,
                            slots,
                        });
                    }
                }
            }
            Kind::Crafting { grid } => {
                for (s, stack) in theirs {
                    grid[s] = stack.map(|h| ItemStack {
                        count: h.count,
                        item: h.item,
                        has_component_changes: h.has_component_changes,
                    });
                }
                let made = ctx.recipes.craft(&grid[1..10], 3);
                if made != grid[0] {
                    grid[0] = made.clone();
                    self.set_slot(writer, id, 0, made)?;
                }
            }
        }
        Ok(())
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
            let name = containers::item_name(ctx, stack.item).unwrap_or_default();
            let max = crate::interact::max_stack(&name);
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
            Kind::Container { position, .. } => containers::close(ctx, uuid, position),
            Kind::Crafting { grid } => {
                let left: Vec<ItemStack> = grid.into_iter().skip(1).flatten().collect();
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
        let Some(id) = self.open.as_ref().and_then(|o| match o.kind {
            Kind::Container { position: p, .. } if p == position => Some(o.id),
            _ => None,
        }) else {
            return Ok(());
        };
        for (slot, item) in slots {
            self.set_slot(writer, id, slot, item)?;
        }
        Ok(())
    }

    /// The container block is gone.
    pub fn container_gone(
        &mut self,
        writer: &mut PacketWriter,
        position: Position,
    ) -> anyhow::Result<()> {
        if let Some(open) = &self.open
            && matches!(open.kind, Kind::Container { position: p, .. } if p == position)
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
