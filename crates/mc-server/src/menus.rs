//! Windows with slots of their own that are not a container block's: the
//! beacon, anvil, grindstone, stonecutter, smithing table... Inputs live in
//! the window and go back to the player on close; a result slot is worked
//! out from the inputs (minecraft.wiki pages of each block).
//!
//! Items here are plain stacks (item, count, damage): what needs item
//! components (names, enchantments, banner patterns, trims) is not done.

use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use crate::beacons;
use crate::context::{Direct, Event, ServerContext};

/// Which window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Beacon,
    Anvil,
    Grindstone,
    Stonecutter,
    Smithing,
    Loom,
    Cartography,
    Enchanting,
    EnderChest,
}

impl Menu {
    /// The menu a block opens.
    pub fn of_block(name: &str) -> Option<Menu> {
        Some(match name {
            "minecraft:beacon" => Menu::Beacon,
            "minecraft:anvil" | "minecraft:chipped_anvil" | "minecraft:damaged_anvil" => {
                Menu::Anvil
            }
            "minecraft:grindstone" => Menu::Grindstone,
            "minecraft:stonecutter" => Menu::Stonecutter,
            "minecraft:smithing_table" => Menu::Smithing,
            "minecraft:loom" => Menu::Loom,
            "minecraft:cartography_table" => Menu::Cartography,
            "minecraft:enchanting_table" => Menu::Enchanting,
            "minecraft:ender_chest" => Menu::EnderChest,
            _ => return None,
        })
    }

    /// `minecraft:menu` entry.
    pub fn menu(self) -> &'static str {
        match self {
            Menu::Beacon => "minecraft:beacon",
            Menu::Anvil => "minecraft:anvil",
            Menu::Grindstone => "minecraft:grindstone",
            Menu::Stonecutter => "minecraft:stonecutter",
            Menu::Smithing => "minecraft:smithing",
            Menu::Loom => "minecraft:loom",
            Menu::Cartography => "minecraft:cartography_table",
            Menu::Enchanting => "minecraft:enchantment",
            Menu::EnderChest => "minecraft:generic_9x3",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Menu::Beacon => "Beacon",
            Menu::Anvil => "Repair & Name",
            Menu::Grindstone => "Repair & Disenchant",
            Menu::Stonecutter => "Stonecutter",
            Menu::Smithing => "Upgrade Gear",
            Menu::Loom => "Loom",
            Menu::Cartography => "Cartography Table",
            Menu::Enchanting => "Enchant",
            Menu::EnderChest => "Ender Chest",
        }
    }

    fn size(self) -> usize {
        match self {
            Menu::Beacon => 1,
            Menu::Anvil | Menu::Grindstone | Menu::Cartography => 3,
            Menu::Stonecutter | Menu::Enchanting => 2,
            Menu::Smithing | Menu::Loom => 4,
            Menu::EnderChest => 27,
        }
        // Stonecutter: input, result. Enchanting: item, lapis.
    }

    /// The slot a result appears in, if the window has one.
    fn result(self) -> Option<usize> {
        match self {
            Menu::Beacon | Menu::Enchanting | Menu::EnderChest => None,
            Menu::Anvil | Menu::Grindstone | Menu::Cartography => Some(2),
            Menu::Stonecutter => Some(1),
            Menu::Smithing | Menu::Loom => Some(3),
        }
    }
}

/// An open window of this kind.
#[derive(Debug)]
pub struct Machine {
    pub menu: Menu,
    /// The block it is about.
    pub at: Position,
    pub slots: Vec<Option<ItemStack>>,
    /// The numbers last sent to the client (`ContainerSetData`).
    pub data: Vec<i16>,
    /// Stonecutter: the recipe chosen (index among those for the input).
    selected: i32,
    /// Anvil: levels the result costs (0: no result).
    cost: i32,
    /// Anvil: how many of the right-hand stack the result uses up.
    used: i32,
    /// The slots as the server left them last (before the client's click).
    last: Vec<Option<ItemStack>>,
}

/// Max cost the anvil does without being "too expensive".
const ANVIL_LIMIT: i32 = 40;

fn game_mode(ctx: &ServerContext, uuid: u128) -> i32 {
    ctx.players
        .lock()
        .expect("players lock")
        .get(&uuid)
        .map_or(0, |p| p.game_mode)
}

fn levels_of(ctx: &ServerContext, uuid: u128) -> i32 {
    ctx.vitals
        .lock()
        .expect("vitals lock")
        .get(&uuid)
        .map_or(0, |v| v.xp_level)
}

/// Takes experience levels from a player.
fn pay_levels(ctx: &ServerContext, uuid: u128, levels: i32) {
    let packet = {
        let mut all = ctx.vitals.lock().expect("vitals lock");
        all.get_mut(&uuid).map(|v| {
            v.add_xp_levels(-levels);
            v.experience_packet()
        })
    };
    if let Some(packet) = packet {
        let _ = ctx.events.send(Event::Direct {
            uuid,
            packet: Direct::Experience(packet),
        });
    }
}

/// Whether `material` repairs `item` on an anvil.
fn repairs(ctx: &ServerContext, item: i32, material: i32) -> bool {
    let Some(it) = ctx.items.get(item) else {
        return false;
    };
    it.repairable
        .iter()
        .any(|entry| match entry.strip_prefix('#') {
            Some(tag) => ctx
                .recipes
                .item_tags
                .get(tag)
                .is_some_and(|items| items.contains(&material)),
            None => ctx.items.id(entry) == Some(material),
        })
}

fn take_one(slot: &mut Option<ItemStack>, n: i32) {
    if let Some(stack) = slot {
        stack.count -= n;
        if stack.count <= 0 {
            *slot = None;
        }
    }
}

impl Machine {
    pub fn new(ctx: &ServerContext, uuid: u128, menu: Menu, at: Position) -> Self {
        let mut machine = Self {
            menu,
            at,
            slots: vec![None; menu.size()],
            data: Vec::new(),
            selected: -1,
            cost: 0,
            used: 0,
            last: vec![None; menu.size()],
        };
        if menu == Menu::EnderChest
            && let Some(p) = ctx.players.lock().expect("players lock").get(&uuid)
        {
            machine.slots.clone_from(&p.extra.ender);
            machine.slots.resize(27, None);
            machine.last.clone_from(&machine.slots);
        }
        machine.data = machine.values(ctx);
        machine
    }

    /// The numbers the client's window shows.
    pub fn values(&self, ctx: &ServerContext) -> Vec<i16> {
        match self.menu {
            Menu::Beacon => beacons::menu_values(ctx, self.at).to_vec(),
            Menu::Anvil => vec![self.cost as i16],
            Menu::Stonecutter => vec![self.selected as i16],
            // Costs, seed, enchantment clues and level clues: no offers.
            Menu::Enchanting => vec![0, 0, 0, 0, -1, -1, -1, -1, -1, -1],
            _ => Vec::new(),
        }
    }

    /// After the client's click changed `slots`: takes what a taken result
    /// used up, then works out the result again. `taken` says the result
    /// slot was emptied by the click.
    pub fn settle(&mut self, ctx: &ServerContext, uuid: u128, taken: bool) {
        if taken {
            // Take what the recipe says from the inputs as they were, not as
            // the client guessed.
            let result = self.menu.result();
            self.slots.clone_from(&self.last);
            if let Some(r) = result {
                self.slots[r] = None;
            }
            self.take(ctx, uuid);
        }
        self.refresh(ctx, uuid);
        self.last.clone_from(&self.slots);
        self.store(ctx, uuid);
    }

    /// The ender chest keeps what is in it.
    fn store(&self, ctx: &ServerContext, uuid: u128) {
        if self.menu == Menu::EnderChest
            && let Some(p) = ctx.players.lock().expect("players lock").get_mut(&uuid)
        {
            p.extra.ender.clone_from(&self.slots);
        }
    }

    /// A button of the window was pressed (stonecutter recipe).
    pub fn button(&mut self, ctx: &ServerContext, uuid: u128, button: i32) {
        if self.menu == Menu::Stonecutter {
            let count = self.cuts(ctx).len() as i32;
            if (0..count).contains(&button) {
                self.selected = button;
            }
        }
        self.refresh(ctx, uuid);
        self.last.clone_from(&self.slots);
    }

    /// Stonecutter recipes for the input, in the order clients list them.
    fn cuts<'a>(&self, ctx: &'a ServerContext) -> Vec<&'a crate::crafting::Cut> {
        let Some(input) = self.slots.first().and_then(Option::as_ref) else {
            return Vec::new();
        };
        ctx.recipes
            .stonecutting
            .iter()
            .filter(|c| c.input.contains(&input.item))
            .collect()
    }

    fn refresh(&mut self, ctx: &ServerContext, uuid: u128) {
        let Some(result) = self.menu.result() else {
            return;
        };
        let made = match self.menu {
            Menu::Stonecutter => {
                let cuts = self.cuts(ctx);
                if self.selected >= cuts.len() as i32 {
                    self.selected = -1;
                }
                usize::try_from(self.selected)
                    .ok()
                    .and_then(|i| cuts.get(i))
                    .map(|c| c.result.clone())
            }
            Menu::Smithing => self.smithing(ctx).map(|(_, stack)| stack),
            Menu::Grindstone => self.grind(ctx),
            Menu::Anvil => self.anvil(ctx, uuid),
            _ => None,
        };
        self.slots[result] = made;
    }

    /// The smithing recipe the three inputs make, and what it makes (the
    /// base keeps its wear).
    fn smithing<'a>(
        &self,
        ctx: &'a ServerContext,
    ) -> Option<(&'a crate::crafting::Smithing, ItemStack)> {
        let [Some(template), Some(base), Some(addition)] =
            [&self.slots[0], &self.slots[1], &self.slots[2]]
        else {
            return None;
        };
        let recipe = ctx.recipes.smithing.iter().find(|r| {
            r.result.is_some()
                && r.template.contains(&template.item)
                && r.base.contains(&base.item)
                && r.addition.contains(&addition.item)
        })?;
        let mut made = recipe.result.clone()?;
        made.damage = base
            .damage
            .min(ctx.items.max_damage(made.item).saturating_sub(1))
            .max(0);
        Some((recipe, made))
    }

    /// The grindstone's result: two worn items of one kind joined (their
    /// remaining wear added, plus 5% of the maximum).
    fn grind(&self, ctx: &ServerContext) -> Option<ItemStack> {
        let (a, b) = (self.slots[0].as_ref()?, self.slots[1].as_ref()?);
        let max = ctx.items.max_damage(a.item);
        if a.item != b.item || max == 0 {
            return None;
        }
        let remaining = (max - a.damage) + (max - b.damage) + max * 5 / 100;
        Some(ItemStack {
            count: 1,
            damage: (max - remaining.min(max)).max(0),
            ..ItemStack::new(a.item, 1)
        })
    }

    /// The anvil's result and cost (set on `self`): repair by material, or
    /// by a second item of the kind.
    fn anvil(&mut self, ctx: &ServerContext, uuid: u128) -> Option<ItemStack> {
        self.cost = 0;
        self.used = 0;
        let left = self.slots[0].clone()?;
        let right = self.slots[1].clone()?;
        let max = ctx.items.max_damage(left.item);
        if max == 0 || left.damage == 0 {
            return None;
        }
        let mut made = left.clone();
        let mut cost = 0;
        if repairs(ctx, left.item, right.item) {
            let step = (max / 4).max(1);
            let mut used = 0;
            while used < right.count && made.damage > 0 {
                made.damage = (made.damage - step.min(made.damage)).max(0);
                cost += 1;
                used += 1;
            }
            self.used = used;
        } else if right.item == left.item {
            let repair = (max - right.damage) + max * 12 / 100;
            made.damage = (left.damage - repair).max(0);
            cost += 2;
            self.used = right.count;
        } else {
            return None;
        }
        if made.damage == left.damage {
            return None;
        }
        let creative = game_mode(ctx, uuid) == 1;
        if cost >= ANVIL_LIMIT && !creative {
            return None;
        }
        self.cost = cost;
        Some(made)
    }

    /// The result was taken: the inputs are used up as the recipe says
    /// (whatever the client believed), levels are paid.
    fn take(&mut self, ctx: &ServerContext, uuid: u128) {
        match self.menu {
            Menu::Stonecutter => take_one(&mut self.slots[0], 1),
            Menu::Smithing => {
                for i in 0..3 {
                    take_one(&mut self.slots[i], 1);
                }
            }
            Menu::Grindstone => {
                self.slots[0] = None;
                self.slots[1] = None;
            }
            Menu::Anvil => {
                // The client took it believing it cost `cost` levels.
                let cost = self.cost;
                let creative = game_mode(ctx, uuid) == 1;
                if !creative {
                    pay_levels(ctx, uuid, cost.min(levels_of(ctx, uuid)));
                }
                self.slots[0] = None;
                take_one(&mut self.slots[1], self.used);
                if !creative {
                    self.wear_anvil(ctx);
                }
                self.cost = 0;
            }
            _ => {}
        }
    }

    /// Using an anvil may chip it, then damage it, then break it (12%).
    fn wear_anvil(&self, ctx: &ServerContext) {
        if crate::util::random_u64() % 100 >= 12 {
            return;
        }
        let blocks = &ctx.data.blocks;
        let Some(state) = ctx.world.block(self.at, blocks) else {
            return;
        };
        let facing = blocks
            .property(state, "facing")
            .unwrap_or("north")
            .to_owned();
        let next = match blocks.block_name(state) {
            Some("minecraft:anvil") => Some("minecraft:chipped_anvil"),
            Some("minecraft:chipped_anvil") => Some("minecraft:damaged_anvil"),
            Some("minecraft:damaged_anvil") => None,
            _ => return,
        };
        let new = match next {
            Some(name) => blocks.state_id(name, &[("facing", &facing)]),
            None => blocks.default_state("minecraft:air"),
        };
        if let Some(new) = new {
            let mut level = crate::level::WorldLevel::new(ctx);
            let logic = level.logic();
            mc_tick::set_block(
                &mut level,
                logic,
                crate::level::to_pos(self.at),
                new,
                mc_tick::Flags::ALL,
            );
            let changes = level.into_changes();
            let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
            if next.is_none() {
                let _ = ctx.events.send(Event::ContainerGone { position: self.at });
            }
        }
    }

    /// Stacks the window gives back to the player when it closes (every
    /// slot but the result).
    pub fn leftovers(&mut self) -> Vec<ItemStack> {
        if self.menu == Menu::EnderChest {
            return Vec::new();
        }
        let result = self.menu.result();
        self.slots
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| Some(*i) != result)
            .filter_map(|(_, s)| s.take())
            .collect()
    }

    pub fn result_slot(&self) -> Option<usize> {
        self.menu.result()
    }
}
