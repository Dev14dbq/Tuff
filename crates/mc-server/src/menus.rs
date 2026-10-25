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
use mc_nbt::{Compound, Tag};

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
    /// A lectern's book (opened by its block use, not here).
    Lectern,
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
            Menu::Lectern => "minecraft:lectern",
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
            Menu::Lectern => "Lectern",
        }
    }

    fn size(self) -> usize {
        match self {
            Menu::Beacon | Menu::Lectern => 1,
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
            Menu::Beacon | Menu::Enchanting | Menu::EnderChest | Menu::Lectern => None,
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
    /// Stonecutter recipe or loom pattern chosen (index among those offered).
    selected: i32,
    /// Anvil: levels the result costs (0: no result).
    cost: i32,
    /// Anvil: how many of the right-hand stack the result uses up (0: all).
    used: i32,
    /// Anvil: the name typed into the field.
    name: Option<String>,
    /// Enchanting table: the three costs, enchantment clues and their levels.
    offers: ([i32; 3], [i32; 3], [i32; 3]),
    /// Enchanting table: the player's seed (shown masked).
    seed: i32,
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

/// A text component as plain text (custom names), translated in English.
pub fn plain_text(ctx: &ServerContext, tag: &Tag) -> String {
    match tag {
        Tag::String(s) => s.clone(),
        Tag::List(l) => l.iter().map(|t| plain_text(ctx, t)).collect(),
        Tag::Compound(c) => {
            let mut out = match (c.get("text").or_else(|| c.get("")), c.get("translate")) {
                (Some(Tag::String(t)), _) => t.clone(),
                (_, Some(Tag::String(key))) => ctx.commands.lang.get(key).to_owned(),
                _ => String::new(),
            };
            if let Some(extra) = c.get("extra") {
                out.push_str(&plain_text(ctx, extra));
            }
            out
        }
        _ => String::new(),
    }
}

/// The name a stack shows, as plain text: its custom name, else the item's.
pub fn hover_name(ctx: &ServerContext, stack: &ItemStack) -> String {
    if let Some(name) = stack
        .components
        .get("custom_name")
        .or_else(|| stack.components.get("item_name"))
    {
        return plain_text(ctx, name);
    }
    let name = ctx
        .items
        .name(stack.item)
        .unwrap_or("")
        .trim_start_matches("minecraft:");
    for kind in ["item", "block"] {
        let key = format!("{kind}.minecraft.{name}");
        let text = ctx.commands.lang.get(&key);
        if text != key {
            return text.to_owned();
        }
    }
    name.to_owned()
}

/// Bookshelves around an enchanting table that power it (each with nothing
/// in the way), at most 15.
fn shelves(ctx: &ServerContext, at: Position) -> i32 {
    let blocks = &ctx.data.blocks;
    let providers = ctx
        .commands
        .env
        .tag("minecraft:block", "minecraft:enchantment_power_provider");
    let transmitters = ctx
        .commands
        .env
        .tag("minecraft:block", "minecraft:enchantment_power_transmitter");
    let name_at = |dx: i32, dy: i32, dz: i32| {
        ctx.world
            .block(Position::new(at.x + dx, at.y + dy, at.z + dz), blocks)
            .and_then(|s| blocks.block_name(s))
            .unwrap_or("minecraft:air")
            .to_owned()
    };
    let mut n = 0;
    for dy in 0..=1 {
        for dx in -2..=2i32 {
            for dz in -2..=2i32 {
                if dx.abs() != 2 && dz.abs() != 2 {
                    continue;
                }
                if providers.contains(&name_at(dx, dy, dz))
                    && transmitters.contains(&name_at(dx / 2, dy / 2, dz / 2))
                {
                    n += 1;
                }
            }
        }
    }
    n.min(15)
}

fn player_seed(ctx: &ServerContext, uuid: u128) -> i32 {
    ctx.players
        .lock()
        .expect("players lock")
        .get(&uuid)
        .map_or(0, |p| p.extra.enchantment_seed)
}

/// A banner pattern list a loom offers for its pattern slot.
fn loom_patterns(ctx: &ServerContext, pattern_item: Option<&ItemStack>) -> Vec<String> {
    let tag = match pattern_item {
        None => "minecraft:no_item_required".to_owned(),
        Some(p) => match ctx
            .items
            .default_component(p.item, "provides_banner_patterns")
            .and_then(|v| v.as_str())
        {
            Some(t) => t.trim_start_matches('#').to_owned(),
            None => return Vec::new(),
        },
    };
    ctx.commands.env.tag("minecraft:banner_pattern", &tag)
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
            name: None,
            offers: ([0; 3], [-1; 3], [-1; 3]),
            seed: player_seed(ctx, uuid),
            last: vec![None; menu.size()],
        };
        if menu == Menu::EnderChest
            && let Some(p) = ctx.players.lock().expect("players lock").get(&uuid)
        {
            machine.slots.clone_from(&p.extra.ender);
            machine.slots.resize(27, None);
            machine.last.clone_from(&machine.slots);
        }
        if menu == Menu::Lectern
            && let Some(be) = ctx.world.block_entity(at, &ctx.data.blocks)
        {
            if let Some(Tag::Compound(book)) = be.data.get("Book") {
                machine.slots[0] = ItemStack::from_nbt(book);
            }
            machine.selected = match be.data.get("Page") {
                Some(Tag::Int(p)) => *p,
                _ => 0,
            };
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
            Menu::Stonecutter | Menu::Loom | Menu::Lectern => vec![self.selected as i16],
            Menu::Enchanting => {
                let (costs, clues, levels) = self.offers;
                let mut v: Vec<i16> = costs.iter().map(|&c| c as i16).collect();
                v.push((self.seed & -16) as i16);
                v.extend(clues.iter().map(|&c| c as i16));
                v.extend(levels.iter().map(|&c| c as i16));
                v
            }
            _ => Vec::new(),
        }
    }

    /// After the client's click changed `slots`: takes what a taken result
    /// used up, then works out the result again. `taken` says the result
    /// slot was emptied by the click.
    pub fn settle(&mut self, ctx: &ServerContext, uuid: u128, taken: bool) {
        if self.menu == Menu::Lectern {
            // The book is only taken with the button.
            self.slots.clone_from(&self.last);
            return;
        }
        if taken {
            // Take what the recipe says from the inputs as they were, not as
            // the client guessed.
            let result = self.menu.result();
            self.slots.clone_from(&self.last);
            if let Some(r) = result {
                if matches!(self.menu, Menu::Stonecutter | Menu::Loom | Menu::Smithing)
                    && let Some(made) = &self.slots[r]
                {
                    crate::stats::item(ctx, uuid, crate::stats::CRAFTED, made.item, made.count);
                }
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

    /// A button of the window was pressed (stonecutter recipe, loom
    /// pattern, enchantment offer).
    pub fn button(&mut self, ctx: &ServerContext, uuid: u128, button: i32) {
        match self.menu {
            Menu::Stonecutter => {
                let count = self.cuts(ctx).len() as i32;
                if (0..count).contains(&button) {
                    self.selected = button;
                }
            }
            Menu::Loom => {
                let count = loom_patterns(ctx, self.slots[2].as_ref()).len() as i32;
                if (0..count).contains(&button) {
                    self.selected = button;
                }
            }
            Menu::Enchanting => self.enchant(ctx, uuid, button),
            Menu::Lectern => self.lectern(ctx, uuid, button),
            _ => {}
        }
        self.refresh(ctx, uuid);
        self.last.clone_from(&self.slots);
    }

    /// The anvil's name field changed.
    pub fn rename(&mut self, ctx: &ServerContext, uuid: u128, name: &str) {
        if self.menu != Menu::Anvil {
            return;
        }
        // Control characters are dropped; longer names are refused.
        let name: String = name
            .chars()
            .filter(|c| *c >= ' ' && *c != '\u{7f}')
            .collect();
        if name.encode_utf16().count() > 50 || self.name.as_deref() == Some(name.as_str()) {
            return;
        }
        self.name = Some(name);
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
        if self.menu == Menu::Enchanting {
            self.offer(ctx);
            return;
        }
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
            Menu::Smithing => self.smithing(ctx),
            Menu::Grindstone => self.grind(ctx),
            Menu::Anvil => self.anvil(ctx, uuid),
            Menu::Loom => self.loom(ctx),
            Menu::Cartography => self.cartography(ctx),
            _ => None,
        };
        self.slots[result] = made;
    }

    // -- Enchanting table ------------------------------------------------

    /// Works out the three offers for the item in the table.
    fn offer(&mut self, ctx: &ServerContext) {
        self.offers = ([0; 3], [-1; 3], [-1; 3]);
        let Some(item) = self.slots[0].as_ref() else {
            return;
        };
        let enchantable = ctx.items.enchantable(item.item);
        if enchantable <= 0 || !crate::enchants::levels(item).is_empty() {
            return;
        }
        let shelves = shelves(ctx, self.at);
        self.offers = crate::enchants::table_values(
            &ctx.enchantments,
            self.seed,
            shelves,
            item.item,
            enchantable,
        );
    }

    /// The player picked offer `slot`: levels and lapis are paid and the
    /// item is enchanted.
    fn enchant(&mut self, ctx: &ServerContext, uuid: u128, slot: i32) {
        let Ok(i) = usize::try_from(slot) else { return };
        if i > 2 {
            return;
        }
        let Some(item) = self.slots[0].clone() else {
            return;
        };
        let cost = self.offers.0[i];
        let creative = game_mode(ctx, uuid) == 1;
        let lapis = self.slots[1].as_ref().map_or(0, |s| s.count);
        let levels = levels_of(ctx, uuid);
        let paid = slot + 1;
        if cost <= 0 || (!creative && (lapis < paid || levels < paid || levels < cost)) {
            return;
        }
        let enchantable = ctx.items.enchantable(item.item);
        let mut random = mc_worldgen::random::Legacy::new(0);
        let list = crate::enchants::table_offer(
            &ctx.enchantments,
            &mut random,
            self.seed,
            slot,
            item.item,
            enchantable,
            cost,
        );
        if list.is_empty() {
            return;
        }
        let book = ctx.enchantments.is_book(item.item);
        let mut made = item.clone();
        if book && let Some(enchanted) = ctx.enchantments.enchanted_book() {
            made.item = enchanted;
        }
        let key = ctx.enchantments.key(made.item);
        let mut have = crate::enchants::levels_in(&made, key);
        for (e, l) in &list {
            have.retain(|(n, _)| *n != e.id);
            have.push((e.id.clone(), *l));
        }
        crate::enchants::set_levels(&mut made, key, &have);
        crate::criteria::enchanted_item(ctx, uuid, &made, cost);
        self.slots[0] = Some(made);
        crate::stats::custom(ctx, uuid, "enchant_item", 1);
        if !creative {
            pay_levels(ctx, uuid, paid);
            take_one(&mut self.slots[1], paid);
        }
        // A new seed for the next offers.
        let seed = crate::util::random_u64() as i32;
        if let Some(p) = ctx.players.lock().expect("players lock").get_mut(&uuid) {
            p.extra.enchantment_seed = seed;
        }
        self.seed = seed;
        let at = [
            f64::from(self.at.x) + 0.5,
            f64::from(self.at.y) + 0.5,
            f64::from(self.at.z) + 0.5,
        ];
        let pitch = 0.9 + (crate::util::random_u64() % 1000) as f32 / 10000.0;
        crate::fx::sound(
            ctx,
            "block.enchantment_table.use",
            crate::fx::category::BLOCKS,
            at,
            1.0,
            pitch,
        );
    }

    // -- Lectern ---------------------------------------------------------

    /// Turns a page (1 back, 2 on, 100 + n to page n) or takes the book (3).
    fn lectern(&mut self, ctx: &ServerContext, uuid: u128, button: i32) {
        let Some(book) = self.slots[0].clone() else {
            return;
        };
        let blocks = &ctx.data.blocks;
        let Some(mut be) = ctx.world.block_entity(self.at, blocks) else {
            return;
        };
        if button == 3 {
            let left = crate::held::give(ctx, uuid, &book);
            if left > 0 {
                let pos = ctx
                    .players
                    .lock()
                    .expect("players lock")
                    .get(&uuid)
                    .and_then(|p| p.pose)
                    .map_or([0.0; 3], |p| [p.x, p.y + 1.0, p.z]);
                crate::tick::spawn_stack_at(ctx, pos, &book.with_count(left));
            }
            be.data.remove("Book");
            be.data.insert("Page", Tag::Int(0));
            ctx.world.set_block_entity(be, blocks);
            let mut level = crate::level::WorldLevel::new(ctx);
            let logic = level.logic();
            mc_tick::redstone::lectern_reset(&mut level, logic, crate::level::to_pos(self.at));
            let changes = level.into_changes();
            let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
            self.slots[0] = None;
            return;
        }
        let pages = crate::blockuse::book_pages(&book) as i32;
        let page = match button {
            1 => self.selected - 1,
            2 => self.selected + 1,
            b if b >= 100 => b - 100,
            _ => return,
        };
        if page < 0 || page >= pages.max(1) || page == self.selected {
            return;
        }
        self.selected = page;
        be.data.insert("Page", Tag::Int(page));
        ctx.world.set_block_entity(be, blocks);
        // Turning a page gives a redstone pulse.
        let mut level = crate::level::WorldLevel::new(ctx);
        let logic = level.logic();
        mc_tick::redstone::lectern_pulse(&mut level, logic, crate::level::to_pos(self.at));
        let changes = level.into_changes();
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
        ctx.container_updates
            .lock()
            .expect("container updates lock")
            .push(self.at);
    }

    // -- Smithing table --------------------------------------------------

    /// What the three inputs make: an upgrade keeps the base's components;
    /// a trim adds the pattern and the addition's material.
    fn smithing(&self, ctx: &ServerContext) -> Option<ItemStack> {
        let [Some(template), Some(base), Some(addition)] =
            [&self.slots[0], &self.slots[1], &self.slots[2]]
        else {
            return None;
        };
        let recipe = ctx.recipes.smithing.iter().find(|r| {
            r.template.contains(&template.item)
                && r.base.contains(&base.item)
                && r.addition.contains(&addition.item)
        })?;
        if let Some(result) = &recipe.result {
            let mut made = base.with_count(result.count);
            made.item = result.item;
            for (name, value) in result.components.iter() {
                match value {
                    Some(v) => made.components.set(name, v.clone()),
                    None => made.components.set_removed(name),
                }
            }
            return Some(made);
        }
        let pattern = recipe.pattern.clone()?;
        let material = ctx
            .items
            .default_component(addition.item, "provides_trim_material")?
            .as_str()?
            .to_owned();
        let mut trim = Compound::new();
        trim.insert("material", Tag::String(material));
        trim.insert("pattern", Tag::String(pattern));
        let trim = Tag::Compound(trim);
        if base
            .components
            .get("trim")
            .is_some_and(|t| mc_protocol::components::tag_eq(t, &trim))
        {
            return None;
        }
        let mut made = base.with_count(1);
        made.components.set("trim", trim);
        Some(made)
    }

    // -- Grindstone ------------------------------------------------------

    /// Enchantments of a stack (stored ones for books) as the grindstone
    /// and anvil read them.
    fn crafting_levels(ctx: &ServerContext, stack: &ItemStack) -> Vec<(String, i32)> {
        crate::enchants::levels_in(stack, ctx.enchantments.key(stack.item))
    }

    /// Leaves only curses on a stack (a book with none left is a plain
    /// book) and sets its repair cost by the curses kept.
    fn remove_non_curses(ctx: &ServerContext, mut stack: ItemStack) -> ItemStack {
        let key = ctx.enchantments.key(stack.item);
        let mut kept = crate::enchants::levels_in(&stack, key);
        kept.retain(|(n, _)| ctx.enchantments.is_curse(n));
        crate::enchants::set_levels(&mut stack, key, &kept);
        if kept.is_empty()
            && ctx.enchantments.is_enchanted_book(stack.item)
            && let Some(book) = ctx.enchantments.book()
        {
            stack.item = book;
            stack.components.reset("stored_enchantments");
        }
        let mut cost = 0;
        for _ in &kept {
            cost = crate::enchants::increased_repair_cost(cost);
        }
        crate::enchants::set_repair_cost(&mut stack, cost);
        stack
    }

    /// The grindstone's result: one item disenchanted (curses stay), or two
    /// of a kind joined (their remaining wear added, plus 5%).
    fn grind(&self, ctx: &ServerContext) -> Option<ItemStack> {
        let (top, bottom) = (self.slots[0].as_ref(), self.slots[1].as_ref());
        if top.is_some_and(|s| s.count > 1) || bottom.is_some_and(|s| s.count > 1) {
            return None;
        }
        match (top, bottom) {
            (None, None) => None,
            (Some(one), None) | (None, Some(one)) => {
                if Self::crafting_levels(ctx, one).is_empty() {
                    return None;
                }
                Some(Self::remove_non_curses(ctx, one.clone()))
            }
            (Some(a), Some(b)) => {
                if a.item != b.item {
                    return None;
                }
                let max = ctx.items.max_damage(a.item);
                let mut made = if max > 0 {
                    let left = (max - a.damage) + (max - b.damage) + max * 5 / 100;
                    let mut made = a.with_count(1);
                    made.damage = (max - left).max(0);
                    made
                } else {
                    if ctx.items.max_stack(a.item) < 2 || !a.same_kind(b) {
                        return None;
                    }
                    a.with_count(2)
                };
                let key = ctx.enchantments.key(made.item);
                let mut have = crate::enchants::levels_in(&made, key);
                for (name, level) in Self::crafting_levels(ctx, b) {
                    let current = have.iter().find(|(n, _)| *n == name).map_or(0, |(_, l)| *l);
                    if !ctx.enchantments.is_curse(&name) || current == 0 {
                        have.retain(|(n, _)| *n != name);
                        have.push((name, level.max(current)));
                    }
                }
                crate::enchants::set_levels(&mut made, key, &have);
                Some(Self::remove_non_curses(ctx, made))
            }
        }
    }

    /// Experience a grindstone gives back for the enchantments it removed.
    fn grind_xp(&self, ctx: &ServerContext) -> i32 {
        let mut points = 0;
        for stack in self.slots[..2].iter().flatten() {
            for (name, level) in Self::crafting_levels(ctx, stack) {
                if !ctx.enchantments.is_curse(&name)
                    && let Some(e) = ctx.enchantments.get(&name)
                {
                    points += e.min_cost(level);
                }
            }
        }
        if points <= 0 {
            return 0;
        }
        let half = (points + 1) / 2;
        half + (crate::util::random_u64() % half as u64) as i32
    }

    // -- Anvil -----------------------------------------------------------

    /// The anvil's result and cost (set on `self`): repair by material or a
    /// second item of the kind, enchantments joined from it (or from a
    /// book), and the name typed.
    fn anvil(&mut self, ctx: &ServerContext, uuid: u128) -> Option<ItemStack> {
        self.cost = 0;
        self.used = 0;
        let input = self.slots[0].clone()?;
        let creative = game_mode(ctx, uuid) == 1;
        let mut made = input.clone();
        let input_key = ctx.enchantments.key(input.item);
        let mut enchants = crate::enchants::levels_in(&made, input_key);
        let mut extra: i64 = i64::from(crate::enchants::repair_cost(&input));
        let mut cost = 0;
        let addition = self.slots[1].clone();
        let max = ctx.items.max_damage(input.item);
        if let Some(addition) = &addition {
            extra += i64::from(crate::enchants::repair_cost(addition));
            let book = ctx.enchantments.is_enchanted_book(addition.item)
                && !crate::enchants::levels_in(addition, "minecraft:stored_enchantments")
                    .is_empty();
            if max > 0 && repairs(ctx, input.item, addition.item) {
                let mut step = made.damage.min(max / 4);
                if step <= 0 {
                    return None;
                }
                let mut used = 0;
                while step > 0 && used < addition.count {
                    made.damage -= step;
                    cost += 1;
                    used += 1;
                    step = made.damage.min(max / 4);
                }
                self.used = used;
            } else {
                if !book && (made.item != addition.item || max == 0) {
                    return None;
                }
                if max > 0 && !book {
                    let left = max - input.damage;
                    let joined = left + (max - addition.damage) + max * 12 / 100;
                    let damage = (max - joined).max(0);
                    if damage < made.damage {
                        made.damage = damage;
                        cost += 2;
                    }
                }
                let mut any_ok = false;
                let mut any_bad = false;
                for (name, level) in Self::crafting_levels(ctx, addition) {
                    let Some(e) = ctx.enchantments.get(&name) else {
                        continue;
                    };
                    let current = enchants
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map_or(0, |(_, l)| *l);
                    let mut level = if current == level {
                        level + 1
                    } else {
                        level.max(current)
                    };
                    let mut fits = e.supported.contains(&input.item)
                        || creative
                        || ctx.enchantments.is_enchanted_book(input.item);
                    for (other, _) in &enchants {
                        if *other != name && !ctx.enchantments.compatible(&name, other) {
                            fits = false;
                            cost += 1;
                        }
                    }
                    if !fits {
                        any_bad = true;
                        continue;
                    }
                    any_ok = true;
                    level = level.min(e.max_level);
                    enchants.retain(|(n, _)| *n != name);
                    enchants.push((name.clone(), level));
                    let mut each = e.anvil_cost;
                    if book {
                        each = (each / 2).max(1);
                    }
                    cost += each * level;
                    if input.count > 1 {
                        cost = 40;
                    }
                }
                if any_bad && !any_ok {
                    return None;
                }
            }
        }
        let mut rename = 0;
        match self.name.as_deref() {
            Some(name) if !name.trim().is_empty() => {
                if name != hover_name(ctx, &input) {
                    rename = 1;
                    cost += 1;
                    made.components
                        .set("custom_name", Tag::String(name.to_owned()));
                }
            }
            _ => {
                if input.components.get("custom_name").is_some() {
                    rename = 1;
                    cost += 1;
                    made.components.reset("custom_name");
                }
            }
        }
        let mut total = if cost <= 0 {
            0
        } else {
            (extra + i64::from(cost)).clamp(0, i64::from(i32::MAX)) as i32
        };
        if cost <= 0 {
            return None;
        }
        // Only renaming is never too expensive.
        if rename == cost && rename > 0 && total >= ANVIL_LIMIT {
            total = ANVIL_LIMIT - 1;
        }
        self.cost = total;
        if total >= ANVIL_LIMIT && !creative {
            return None;
        }
        let mut repair = crate::enchants::repair_cost(&made);
        if let Some(addition) = &addition {
            repair = repair.max(crate::enchants::repair_cost(addition));
        }
        if rename != cost || rename == 0 {
            repair = crate::enchants::increased_repair_cost(repair);
        }
        crate::enchants::set_repair_cost(&mut made, repair);
        crate::enchants::set_levels(&mut made, input_key, &enchants);
        Some(made)
    }

    // -- Loom ------------------------------------------------------------

    /// A banner with the chosen pattern added in the dye's colour.
    fn loom(&mut self, ctx: &ServerContext) -> Option<ItemStack> {
        let (Some(banner), Some(dye)) = (self.slots[0].as_ref(), self.slots[1].as_ref()) else {
            self.selected = -1;
            return None;
        };
        let patterns = loom_patterns(ctx, self.slots[2].as_ref());
        if self.selected >= patterns.len() as i32 {
            self.selected = -1;
        }
        let pattern = patterns.get(usize::try_from(self.selected).ok()?)?;
        let color = ctx.items.default_component(dye.item, "dye")?.as_str()?;
        let mut layers = match banner.components.get("banner_patterns") {
            Some(Tag::List(l)) => l.clone(),
            _ => Vec::new(),
        };
        if layers.len() >= 6 {
            return None;
        }
        let mut layer = Compound::new();
        layer.insert("pattern", Tag::String(pattern.clone()));
        layer.insert("color", Tag::String(color.to_owned()));
        layers.push(Tag::Compound(layer));
        let mut made = banner.with_count(1);
        made.components.set("banner_patterns", Tag::List(layers));
        Some(made)
    }

    // -- Cartography table -----------------------------------------------

    /// A map copied (with an empty map), zoomed out (paper) or locked
    /// (glass pane).
    fn cartography(&self, ctx: &ServerContext) -> Option<ItemStack> {
        let (map, other) = (self.slots[0].as_ref()?, self.slots[1].as_ref()?);
        if ctx.items.name(map.item) != Some("minecraft:filled_map") {
            return None;
        }
        let state = crate::maps::state(ctx, map);
        match ctx.items.name(other.item)? {
            "minecraft:map" => Some(map.with_count(2)),
            "minecraft:paper" => {
                if state.is_some_and(|s| s.locked || s.scale >= 4) {
                    return None;
                }
                let mut made = map.with_count(1);
                made.components
                    .set("map_post_processing", Tag::from("scale"));
                Some(made)
            }
            "minecraft:glass_pane" => {
                if state.is_some_and(|s| s.locked) {
                    return None;
                }
                let mut made = map.with_count(1);
                made.components
                    .set("map_post_processing", Tag::from("lock"));
                Some(made)
            }
            _ => None,
        }
    }

    /// The result was taken: the inputs are used up as the recipe says
    /// (whatever the client believed), levels are paid.
    fn take(&mut self, ctx: &ServerContext, uuid: u128) {
        let centre = [
            f64::from(self.at.x) + 0.5,
            f64::from(self.at.y) + 0.5,
            f64::from(self.at.z) + 0.5,
        ];
        match self.menu {
            Menu::Stonecutter => take_one(&mut self.slots[0], 1),
            Menu::Smithing => {
                let inputs: Vec<ItemStack> = self.slots[..3].iter().flatten().cloned().collect();
                if let [template, base, addition] = &inputs[..]
                    && let Some(r) = ctx.recipes.smithing.iter().find(|r| {
                        r.template.contains(&template.item)
                            && r.base.contains(&base.item)
                            && r.addition.contains(&addition.item)
                    })
                {
                    crate::criteria::crafted(ctx, uuid, "minecraft:recipe_crafted", &r.id, &inputs);
                }
                for i in 0..3 {
                    take_one(&mut self.slots[i], 1);
                }
            }
            Menu::Grindstone => {
                let xp = self.grind_xp(ctx);
                if xp > 0 {
                    crate::xp::spawn(ctx, centre, xp);
                }
                crate::fx::level_event(ctx, 1042, centre, 0);
                self.slots[0] = None;
                self.slots[1] = None;
            }
            Menu::Loom => {
                take_one(&mut self.slots[0], 1);
                take_one(&mut self.slots[1], 1);
            }
            Menu::Cartography => {
                take_one(&mut self.slots[0], 1);
                take_one(&mut self.slots[1], 1);
            }
            Menu::Anvil => {
                // The client took it believing it cost `cost` levels.
                let cost = self.cost;
                let creative = game_mode(ctx, uuid) == 1;
                if !creative {
                    pay_levels(ctx, uuid, cost.min(levels_of(ctx, uuid)));
                }
                self.slots[0] = None;
                if self.used > 0 {
                    take_one(&mut self.slots[1], self.used);
                } else {
                    self.slots[1] = None;
                }
                if creative || !self.wear_anvil(ctx) {
                    crate::fx::level_event(ctx, 1030, centre, 0);
                }
                self.cost = 0;
                self.name = None;
            }
            _ => {}
        }
    }

    /// Using an anvil may chip it, then damage it, then break it (12%).
    /// Whether it did.
    fn wear_anvil(&self, ctx: &ServerContext) -> bool {
        if crate::util::random_u64() % 100 >= 12 {
            return false;
        }
        let blocks = &ctx.data.blocks;
        let Some(state) = ctx.world.block(self.at, blocks) else {
            return false;
        };
        let facing = blocks
            .property(state, "facing")
            .unwrap_or("north")
            .to_owned();
        let next = match blocks.block_name(state) {
            Some("minecraft:anvil") => Some("minecraft:chipped_anvil"),
            Some("minecraft:chipped_anvil") => Some("minecraft:damaged_anvil"),
            Some("minecraft:damaged_anvil") => None,
            _ => return false,
        };
        let new = match next {
            Some(name) => blocks.state_id(name, &[("facing", &facing)]),
            None => blocks.default_state("minecraft:air"),
        };
        let centre = [
            f64::from(self.at.x) + 0.5,
            f64::from(self.at.y) + 0.5,
            f64::from(self.at.z) + 0.5,
        ];
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
                crate::fx::level_event(ctx, 1029, centre, 0);
                let _ = ctx.events.send(Event::ContainerGone { position: self.at });
            } else {
                crate::fx::level_event(ctx, 1030, centre, 0);
            }
        }
        true
    }

    /// Stacks the window gives back to the player when it closes (every
    /// slot but the result).
    pub fn leftovers(&mut self) -> Vec<ItemStack> {
        if matches!(self.menu, Menu::EnderChest | Menu::Lectern) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick::lock;

    const P: u128 = 9;

    fn setup() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let ctx = ServerContext::new(crate::Config::new(dir), data).ok()?;
        let mut v = crate::vitals::Vitals::default();
        v.xp_level = 30;
        lock(&ctx.vitals).insert(P, v);
        Some(ctx)
    }

    fn stack(ctx: &ServerContext, name: &str, count: i32) -> ItemStack {
        ItemStack::new(ctx.items.id(name).unwrap(), count)
    }

    fn enchanted(ctx: &ServerContext, name: &str, list: &[(&str, i32)]) -> ItemStack {
        let mut s = stack(ctx, name, 1);
        let key = ctx.enchantments.key(s.item);
        let list: Vec<(String, i32)> = list
            .iter()
            .map(|(n, l)| (format!("minecraft:{n}"), *l))
            .collect();
        crate::enchants::set_levels(&mut s, key, &list);
        s
    }

    fn machine(ctx: &ServerContext, menu: Menu, slots: Vec<Option<ItemStack>>) -> Machine {
        let mut m = Machine::new(ctx, P, menu, Position::new(0, -60, 0));
        for (i, s) in slots.into_iter().enumerate() {
            m.slots[i] = s;
        }
        m.refresh(ctx, P);
        m
    }

    fn levels(ctx: &ServerContext, s: &ItemStack) -> Vec<(String, i32)> {
        let mut l = crate::enchants::levels_in(s, ctx.enchantments.key(s.item));
        l.sort();
        l
    }

    #[test]
    fn anvil_renames_for_one_level() {
        let Some(ctx) = setup() else { return };
        let mut m = machine(
            &ctx,
            Menu::Anvil,
            vec![Some(stack(&ctx, "minecraft:diamond_sword", 1))],
        );
        assert_eq!(m.slots[2], None);
        m.rename(&ctx, P, "Excalibur");
        let made = m.slots[2].clone().unwrap();
        assert_eq!(
            made.components.get("custom_name"),
            Some(&Tag::from("Excalibur"))
        );
        assert_eq!(m.cost, 1);
        // Renaming alone does not make later work dearer.
        assert_eq!(crate::enchants::repair_cost(&made), 0);
        // The item's own name is no rename.
        m.rename(&ctx, P, "Diamond Sword");
        assert_eq!(m.slots[2], None);
    }

    #[test]
    fn anvil_joins_enchantments() {
        let Some(ctx) = setup() else { return };
        let sword = |l| enchanted(&ctx, "minecraft:diamond_sword", &[("sharpness", l)]);
        let m = machine(&ctx, Menu::Anvil, vec![Some(sword(4)), Some(sword(4))]);
        let made = m.slots[2].clone().unwrap();
        assert_eq!(levels(&ctx, &made), [("minecraft:sharpness".to_owned(), 5)]);
        // Sharpness costs 1 per level.
        assert_eq!(m.cost, 5);
        let book = enchanted(&ctx, "minecraft:enchanted_book", &[("unbreaking", 3)]);
        let m = machine(&ctx, Menu::Anvil, vec![Some(sword(1)), Some(book)]);
        let made = m.slots[2].clone().unwrap();
        assert_eq!(
            levels(&ctx, &made),
            [
                ("minecraft:sharpness".to_owned(), 1),
                ("minecraft:unbreaking".to_owned(), 3)
            ]
        );
        // Unbreaking's anvil cost 2, halved from a book: 1 × 3.
        assert_eq!(m.cost, 3);
        let smite = enchanted(&ctx, "minecraft:enchanted_book", &[("smite", 1)]);
        let m = machine(&ctx, Menu::Anvil, vec![Some(sword(1)), Some(smite)]);
        assert_eq!(m.slots[2], None, "smite does not go with sharpness");
    }

    #[test]
    fn anvil_repairs_with_material() {
        let Some(ctx) = setup() else { return };
        let mut sword = stack(&ctx, "minecraft:diamond_sword", 1);
        sword.damage = 1000;
        let m = machine(
            &ctx,
            Menu::Anvil,
            vec![Some(sword), Some(stack(&ctx, "minecraft:diamond", 5))],
        );
        let made = m.slots[2].clone().unwrap();
        // A quarter of 1561 per diamond (390): three mend it all.
        assert_eq!(made.damage, 0);
        assert_eq!((m.cost, m.used), (3, 3));
    }

    #[test]
    fn grindstone_keeps_curses() {
        let Some(ctx) = setup() else { return };
        let sword = enchanted(
            &ctx,
            "minecraft:diamond_sword",
            &[("sharpness", 3), ("vanishing_curse", 1)],
        );
        let m = machine(&ctx, Menu::Grindstone, vec![Some(sword)]);
        let made = m.slots[2].clone().unwrap();
        assert_eq!(
            levels(&ctx, &made),
            [("minecraft:vanishing_curse".to_owned(), 1)]
        );
        assert_eq!(crate::enchants::repair_cost(&made), 1);
        assert!(m.grind_xp(&ctx) > 0);
        let book = enchanted(&ctx, "minecraft:enchanted_book", &[("mending", 1)]);
        let m = machine(&ctx, Menu::Grindstone, vec![None, Some(book)]);
        let made = m.slots[2].clone().unwrap();
        assert_eq!(ctx.items.name(made.item), Some("minecraft:book"));
        assert!(made.components.is_empty());
        let plain = stack(&ctx, "minecraft:diamond_sword", 1);
        let m = machine(&ctx, Menu::Grindstone, vec![Some(plain)]);
        assert_eq!(m.slots[2], None);
    }

    #[test]
    fn smithing_trims_and_upgrades_keep_components() {
        let Some(ctx) = setup() else { return };
        let m = machine(
            &ctx,
            Menu::Smithing,
            vec![
                Some(stack(
                    &ctx,
                    "minecraft:coast_armor_trim_smithing_template",
                    1,
                )),
                Some(stack(&ctx, "minecraft:iron_chestplate", 1)),
                Some(stack(&ctx, "minecraft:gold_ingot", 1)),
            ],
        );
        let made = m.slots[3].clone().unwrap();
        let Some(Tag::Compound(trim)) = made.components.get("trim") else {
            panic!("no trim")
        };
        assert_eq!(trim.get("material"), Some(&Tag::from("minecraft:gold")));
        assert_eq!(trim.get("pattern"), Some(&Tag::from("minecraft:coast")));
        let mut sword = enchanted(&ctx, "minecraft:diamond_sword", &[("sharpness", 2)]);
        sword.damage = 7;
        let m = machine(
            &ctx,
            Menu::Smithing,
            vec![
                Some(stack(
                    &ctx,
                    "minecraft:netherite_upgrade_smithing_template",
                    1,
                )),
                Some(sword),
                Some(stack(&ctx, "minecraft:netherite_ingot", 1)),
            ],
        );
        let made = m.slots[3].clone().unwrap();
        assert_eq!(ctx.items.name(made.item), Some("minecraft:netherite_sword"));
        assert_eq!(made.damage, 7);
        assert_eq!(levels(&ctx, &made), [("minecraft:sharpness".to_owned(), 2)]);
    }

    #[test]
    fn enchanting_table_enchants_a_book() {
        let Some(ctx) = setup() else { return };
        crate::stats::load(&ctx, P);
        let mut m = machine(
            &ctx,
            Menu::Enchanting,
            vec![
                Some(stack(&ctx, "minecraft:book", 1)),
                Some(stack(&ctx, "minecraft:lapis_lazuli", 3)),
            ],
        );
        let (costs, clues, _) = m.offers;
        assert!(costs[0] > 0, "{costs:?}");
        assert!(clues[0] >= 0);
        m.button(&ctx, P, 0);
        assert_eq!(
            crate::stats::get(&ctx, P, crate::stats::CUSTOM, "enchant_item"),
            1
        );
        let made = m.slots[0].clone().unwrap();
        assert_eq!(ctx.items.name(made.item), Some("minecraft:enchanted_book"));
        assert!(!levels(&ctx, &made).is_empty());
        assert_eq!(m.slots[1].as_ref().map(|s| s.count), Some(2));
        assert_eq!(lock(&ctx.vitals)[&P].xp_level, 29);
        // An enchanted item gets no offers.
        assert_eq!(m.offers.0, [0; 3]);
    }

    #[test]
    fn loom_adds_a_pattern() {
        let Some(ctx) = setup() else { return };
        let mut m = machine(
            &ctx,
            Menu::Loom,
            vec![
                Some(stack(&ctx, "minecraft:white_banner", 1)),
                Some(stack(&ctx, "minecraft:red_dye", 1)),
            ],
        );
        assert_eq!(m.slots[3], None);
        m.button(&ctx, P, 0);
        let made = m.slots[3].clone().unwrap();
        let Some(Tag::List(layers)) = made.components.get("banner_patterns") else {
            panic!()
        };
        let Tag::Compound(layer) = &layers[0] else {
            panic!()
        };
        assert_eq!(layer.get("color"), Some(&Tag::from("red")));
    }
}
