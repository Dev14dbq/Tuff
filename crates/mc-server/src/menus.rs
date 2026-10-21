//! Windows with slots of their own that are not a container block's: the
//! beacon, and the anvil, grindstone... (inputs live in the window and go back
//! to the player on close; a result slot is worked out from the inputs).

use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use crate::beacons;
use crate::context::ServerContext;

/// Which window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Beacon,
}

impl Menu {
    /// The block a menu belongs to.
    pub fn of_block(name: &str) -> Option<Menu> {
        match name {
            "minecraft:beacon" => Some(Menu::Beacon),
            _ => None,
        }
    }

    /// `minecraft:menu` entry.
    pub fn menu(self) -> &'static str {
        match self {
            Menu::Beacon => "minecraft:beacon",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Menu::Beacon => "Beacon",
        }
    }

    fn size(self) -> usize {
        match self {
            Menu::Beacon => 1,
        }
    }

    /// The slot a result appears in, if the window has one.
    fn result(self) -> Option<usize> {
        match self {
            Menu::Beacon => None,
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
}

impl Machine {
    pub fn new(ctx: &ServerContext, menu: Menu, at: Position) -> Self {
        let mut machine = Self {
            menu,
            at,
            slots: vec![None; menu.size()],
            data: Vec::new(),
        };
        machine.data = machine.values(ctx);
        machine
    }

    /// The numbers the client's window shows.
    pub fn values(&self, ctx: &ServerContext) -> Vec<i16> {
        match self.menu {
            Menu::Beacon => beacons::menu_values(ctx, self.at).to_vec(),
        }
    }

    /// After the client's click changed `slots`: works out results and
    /// takes what a taken result used up. `taken` says the result slot was
    /// emptied by the click.
    pub fn settle(&mut self, _ctx: &ServerContext, _uuid: u128, _taken: bool) {}

    /// Stacks the window gives back to the player when it closes (every
    /// slot but the result).
    pub fn leftovers(&mut self) -> Vec<ItemStack> {
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
