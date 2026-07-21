//! House **superweapons** — the charge/ready/fire cycle and the timed effects
//! (nuclear strike, iron curtain, chronosphere). A faithful port of the original
//! `SuperClass` (`super.cpp`) plus the `HouseClass::Place_Special_Blast` effect
//! dispatch (`house.cpp:2777-3085`), expressed as plain deterministic data.
//!
//! **Design (`SuperClass`, `super.cpp`).** A superweapon is *present* while its
//! house owns the granting structure (MSLO→nuclear, IRON→iron curtain,
//! PDOX→chronosphere — a per-tick function of `ActiveBScan`, not a one-time grant;
//! `house.cpp:1598/1667/1750`). While present and not ready it **charges**: a
//! `Control` countdown ticks down from `RechargeTime` to zero, then it is *ready*
//! (`SuperClass::AI`, `super.cpp:265`). Charging is **suspended** whenever the
//! house lacks full power (`Suspend(Power_Fraction() < 1)`, `house.cpp:1484`).
//! Firing consumes the charge and restarts the countdown (`Discharged`,
//! `super.cpp:233`).
//!
//! The state lives in a `Vec<SuperWeapon>` on [`crate::World`], iterated in slot
//! order and folded into the state hash **only when non-empty** — so every world
//! without a superweapon building (all prior goldens) is byte-identical.

use crate::coords::CellCoord;
use crate::hash::Fnv1a;

/// Which superweapon this is. The set we model (nuclear / iron curtain /
/// chronosphere); GPS / paratroopers / parabombs are deferred (QUIRKS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuperKind {
    /// Nuclear missile (Missile Silo, MSLO → `SPC_NUCLEAR_BOMB`). Target a cell →
    /// after a fall delay a `WARHEAD_NUKE` blast devastates the area.
    Nuclear,
    /// Iron curtain (IRON → `SPC_IRON_CURTAIN`). Target a unit/building → temporary
    /// invulnerability (`IronCurtainCountDown`).
    IronCurtain,
    /// Chronosphere (PDOX → `SPC_CHRONOSPHERE`). Target a vehicle + destination →
    /// teleport it there (`DriveClass::Teleport_To`).
    Chronosphere,
}

impl SuperKind {
    /// The superweapon a building type grants, by catalog short-name, or `None`
    /// for an ordinary structure (`house.cpp` `STRUCTF_*` gates).
    pub fn for_building(name: &str) -> Option<SuperKind> {
        match name.trim().to_ascii_uppercase().as_str() {
            "MSLO" => Some(SuperKind::Nuclear),
            "IRON" => Some(SuperKind::IronCurtain),
            "PDOX" => Some(SuperKind::Chronosphere),
            _ => None,
        }
    }

    /// Recharge time in **minutes** (`[Recharge]`, rules.ini): Nuke 13, IronCurtain
    /// 11, Chrono 7. Used to seed [`SuperWeapon::recharge`] in ticks.
    pub fn recharge_minutes(self) -> i32 {
        match self {
            SuperKind::Nuclear => 13,
            SuperKind::IronCurtain => 11,
            SuperKind::Chronosphere => 7,
        }
    }

    fn tag(self) -> u8 {
        match self {
            SuperKind::Nuclear => 0,
            SuperKind::IronCurtain => 1,
            SuperKind::Chronosphere => 2,
        }
    }
}

/// One house's charge state for one superweapon (`SuperClass`, `super.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuperWeapon {
    /// Owning house.
    pub house: u8,
    /// Which superweapon.
    pub kind: SuperKind,
    /// Total charge time in ticks (`RechargeTime`).
    pub recharge: i32,
    /// Remaining charge ticks (`Control`); `0` once fully charged.
    pub control: i32,
    /// Fully charged and awaiting a fire order (`IsReady`).
    pub ready: bool,
}

impl SuperWeapon {
    /// A freshly-enabled superweapon: begins charging from full recharge time
    /// (`SuperClass::Enable` → `Recharge`, `super.cpp:139/194`).
    pub fn new(house: u8, kind: SuperKind, recharge: i32) -> SuperWeapon {
        SuperWeapon {
            house,
            kind,
            recharge: recharge.max(1),
            control: recharge.max(1),
            ready: false,
        }
    }

    /// Advance one tick's charge unless suspended (`SuperClass::AI`,
    /// `super.cpp:265`): when powered, decrement `control`; at zero become ready.
    pub fn charge_tick(&mut self, suspended: bool) {
        if self.ready || suspended {
            return;
        }
        if self.control > 0 {
            self.control -= 1;
        }
        if self.control == 0 {
            self.ready = true;
        }
    }

    /// Fire: consume the charge and restart the countdown (`Discharged`,
    /// `super.cpp:233`). Returns whether it was ready (i.e. actually fired).
    pub fn discharge(&mut self) -> bool {
        if !self.ready {
            return false;
        }
        self.ready = false;
        self.control = self.recharge;
        true
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u8(self.house);
        h.write_u8(self.kind.tag());
        h.write_i32(self.recharge);
        h.write_i32(self.control);
        h.write_u8(self.ready as u8);
    }
}

/// A nuclear strike in flight: the missile has launched and will detonate at
/// `cell` when `timer` reaches zero (the `BULLET_NUKE_DOWN` fall,
/// `house.cpp:2818`). Stored on [`crate::World`] and ticked by the superweapon
/// system; folded into the hash only while any strike is pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NukeStrike {
    /// Ground-zero cell.
    pub cell: CellCoord,
    /// Firing house (blast attribution / kill credit).
    pub house: u8,
    /// Ticks until detonation.
    pub timer: u16,
}

impl NukeStrike {
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_i32(self.cell.x);
        h.write_i32(self.cell.y);
        h.write_u8(self.house);
        h.write_u16(self.timer);
    }
}
