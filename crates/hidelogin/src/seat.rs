//! The seat, as seatd keeps it (seatd/seat.c), as a state machine: what
//! clients there are, which one has the devices, and what to do when a VT
//! is switched. Nothing here opens a device or touches a terminal; each
//! call returns the [`Effect`]s the daemon then carries out, in order.
//!
//! hideOS's seat is seat0 and bound to VTs: one client per VT, and the
//! client on the VT the kernel shows is the one with the devices. A client
//! is a compositor — cosmic-comp, for the greeter or a person's session —
//! and who may become one is the daemon's to decide before
//! [`Seat::open_seat`]: a process of the login session on that VT.

use std::collections::BTreeMap;

use crate::seatd::Reply;

pub type ClientId = u64;

/// The most devices one client may hold, as seatd's MAX_SEAT_DEVICES.
pub const MAX_DEVICES: usize = 128;

const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const EBADF: i32 = 9;
const EBUSY: i32 = 16;
const EINVAL: i32 = 22;
const EMFILE: i32 = 24;

/// The devices a client may ask for, by their canonical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// `/dev/dri/*`: the client is made DRM master while active.
    Drm,
    /// `/dev/input/event*`: revoked for good when the client is disabled.
    Evdev,
    /// `/dev/hidraw*`: as evdev.
    Hidraw,
}

/// What kind of device `path`, already canonical, is; `None` for anything a
/// compositor has no business opening through the seat.
pub fn device_kind(path: &str) -> Option<DeviceKind> {
    let named = |prefix: &str| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
    };
    if named("/dev/dri/") {
        Some(DeviceKind::Drm)
    } else if named("/dev/input/event") {
        Some(DeviceKind::Evdev)
    } else if named("/dev/hidraw") {
        Some(DeviceKind::Hidraw)
    } else {
        None
    }
}

/// What the daemon does for the seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Send(ClientId, Reply),
    /// Make an open device usable again: DRM master. Evdev and hidraw
    /// devices are not reactivated — revoked, they are reopened.
    Activate(ClientId, i32),
    /// Take a device from its client: DRM master dropped, evdev and hidraw
    /// revoked.
    Deactivate(ClientId, i32),
    /// Deactivate, then close the descriptor.
    Close(ClientId, i32),
    /// Give the VT to a graphical client: switching by the process, the
    /// keyboard off, graphics mode.
    OpenVt(i32),
    /// Give the VT back to the console.
    CloseVt(i32),
    /// Ask the kernel to switch from the current VT to another.
    SwitchVt {
        from: i32,
        to: i32,
    },
    /// Let the kernel go on with a switch away from (`release`) or to the
    /// current VT.
    AckVt {
        release: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    New,
    Active,
    PendingDisable,
    Disabled,
}

#[derive(Debug)]
struct Device {
    id: i32,
    path: String,
    kind: DeviceKind,
    refs: u32,
    active: bool,
}

#[derive(Debug)]
struct Client {
    /// The VT the client belongs to: seatd's "session".
    vt: i32,
    state: State,
    devices: Vec<Device>,
}

/// What [`Seat::open_device`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Open {
    /// The client has it open already, under this id: the same descriptor
    /// goes back to it.
    Again(i32),
    /// Open it, as `kind`; then [`Seat::opened`] or [`Seat::open_failed`].
    New { id: i32, kind: DeviceKind },
}

#[derive(Debug, Default)]
pub struct Seat {
    /// The VT the kernel shows, when known: `None` between a release and
    /// the next acquire.
    current_vt: Option<i32>,
    /// In the order they joined: the first eligible one is activated.
    clients: BTreeMap<ClientId, Client>,
    joined: Vec<ClientId>,
    active: Option<ClientId>,
    next: Option<ClientId>,
}

impl Seat {
    pub fn new() -> Seat {
        Seat::default()
    }

    /// The client with the devices, if any.
    pub fn active(&self) -> Option<ClientId> {
        self.active
    }

    /// The VT `client` belongs to.
    pub fn vt_of(&self, client: ClientId) -> Option<i32> {
        self.clients.get(&client).map(|c| c.vt)
    }

    /// A client asks for the seat, `current_vt` being the VT shown now. It
    /// joins as that VT's client and, with no client active, becomes it.
    pub fn open_seat(&mut self, client: ClientId, current_vt: i32) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.clients.contains_key(&client) {
            effects.push(Effect::Send(client, Reply::Error(EBUSY)));
            return effects;
        }
        if let Some(active) = self.active.and_then(|id| self.clients.get(&id)) {
            if active.state != State::PendingDisable {
                effects.push(Effect::Send(client, Reply::Error(EBUSY)));
                return effects;
            }
            if self.clients.values().any(|c| c.vt == current_vt) {
                effects.push(Effect::Send(client, Reply::Error(EBUSY)));
                return effects;
            }
        }
        self.current_vt = Some(current_vt);
        self.clients.insert(
            client,
            Client {
                vt: current_vt,
                state: State::New,
                devices: Vec::new(),
            },
        );
        self.joined.push(client);
        effects.push(Effect::Send(client, Reply::SeatOpened("seat0".to_owned())));
        self.enable(client, &mut effects);
        effects
    }

    /// The client leaves the seat, asked or not: on a close request, or
    /// when its connection ends. `asked` sends `SeatClosed`.
    pub fn close_seat(&mut self, client: ClientId, asked: bool) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(mut gone) = self.clients.remove(&client) else {
            if asked {
                effects.push(Effect::Send(client, Reply::Error(EINVAL)));
            }
            return effects;
        };
        self.joined.retain(|&c| c != client);
        if self.next == Some(client) {
            self.next = None;
        }
        for device in gone.devices.drain(..) {
            effects.push(Effect::Close(client, device.id));
        }
        let was_active = self.active == Some(client);
        if was_active {
            self.active = None;
            self.activate(&mut effects);
            if self.active.is_none() {
                effects.push(Effect::CloseVt(gone.vt));
            }
        } else if gone.state != State::New {
            effects.push(Effect::CloseVt(gone.vt));
        }
        if asked {
            effects.push(Effect::Send(client, Reply::SeatClosed));
        }
        effects
    }

    /// A request for the device at `path`, canonical. Only the active
    /// client may open one.
    pub fn open_device(&mut self, client: ClientId, path: &str) -> Result<Open, i32> {
        let Some(c) = self.clients.get_mut(&client) else {
            return Err(EINVAL);
        };
        if c.state != State::Active {
            return Err(EPERM);
        }
        let kind = device_kind(path).ok_or(ENOENT)?;
        if let Some(device) = c.devices.iter_mut().find(|d| d.path == path) {
            device.refs += 1;
            return Ok(Open::Again(device.id));
        }
        if c.devices.len() >= MAX_DEVICES {
            return Err(EMFILE);
        }
        let id = c.devices.iter().map(|d| d.id).max().unwrap_or(0) + 1;
        Ok(Open::New { id, kind })
    }

    /// The daemon opened `path` as device `id` for `client`.
    pub fn opened(&mut self, client: ClientId, id: i32, path: &str, kind: DeviceKind) {
        if let Some(c) = self.clients.get_mut(&client) {
            c.devices.push(Device {
                id,
                path: path.to_owned(),
                kind,
                refs: 1,
                active: true,
            });
        }
    }

    /// One reference to device `id` given back; the last one closes it.
    pub fn close_device(&mut self, client: ClientId, id: i32) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(c) = self.clients.get_mut(&client) else {
            effects.push(Effect::Send(client, Reply::Error(EINVAL)));
            return effects;
        };
        let Some(at) = c.devices.iter().position(|d| d.id == id) else {
            effects.push(Effect::Send(client, Reply::Error(EBADF)));
            return effects;
        };
        if let Some(device) = c.devices.get_mut(at) {
            device.refs = device.refs.saturating_sub(1);
            if device.refs == 0 {
                c.devices.remove(at);
                effects.push(Effect::Close(client, id));
            }
        }
        effects.push(Effect::Send(client, Reply::DeviceClosed));
        effects
    }

    /// The client acknowledges that it let go of its devices.
    pub fn disable_acked(&mut self, client: ClientId) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(c) = self.clients.get_mut(&client) else {
            effects.push(Effect::Send(client, Reply::Error(EINVAL)));
            return effects;
        };
        if c.state != State::PendingDisable {
            effects.push(Effect::Send(client, Reply::Error(EBUSY)));
            return effects;
        }
        c.state = State::Disabled;
        effects.push(Effect::Send(client, Reply::SeatDisabled));
        if self.active == Some(client) {
            self.active = None;
            self.activate(&mut effects);
        }
        effects
    }

    /// The active client asks to switch to another VT.
    pub fn switch_session(&mut self, client: ClientId, vt: i32) -> Vec<Effect> {
        let mut effects = Vec::new();
        let state = self.clients.get(&client).map(|c| (c.state, c.vt));
        let reply = match state {
            Some((State::Active, own)) if vt > 0 => {
                if vt != own
                    && self.next.is_none()
                    && let Some(from) = self.current_vt
                {
                    effects.push(Effect::SwitchVt { from, to: vt });
                }
                Reply::SessionSwitched
            }
            Some((State::Active, _)) => Reply::Error(EINVAL),
            _ => Reply::Error(EPERM),
        };
        effects.push(Effect::Send(client, reply));
        effects
    }

    /// The kernel asks to switch away from the current VT: the active
    /// client is disabled, and the switch goes on.
    pub fn vt_release(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(active) = self.active {
            self.disable(active, &mut effects);
        }
        effects.push(Effect::AckVt { release: true });
        self.current_vt = None;
        effects
    }

    /// The kernel switched to VT `vt`: its client, if any, gets the seat.
    pub fn vt_acquire(&mut self, vt: i32) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.current_vt = Some(vt);
        effects.push(Effect::AckVt { release: false });
        if self.active.is_none() {
            self.activate(&mut effects);
        }
        effects
    }

    fn activate(&mut self, effects: &mut Vec<Effect>) {
        if self.active.is_some() {
            return;
        }
        let candidate = match self.next.take() {
            Some(next) => Some(next),
            None => self.current_vt.and_then(|vt| {
                self.joined
                    .iter()
                    .copied()
                    .find(|id| self.clients.get(id).is_some_and(|c| c.vt == vt))
            }),
        };
        if let Some(client) = candidate {
            self.enable(client, effects);
        }
    }

    fn enable(&mut self, client: ClientId, effects: &mut Vec<Effect>) {
        if self.active.is_some() {
            return;
        }
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        if c.state != State::New && c.state != State::Disabled {
            return;
        }
        effects.push(Effect::OpenVt(c.vt));
        for device in &mut c.devices {
            // A revoked input device stays revoked: the client reopens it.
            if !device.active && device.kind == DeviceKind::Drm {
                device.active = true;
                effects.push(Effect::Activate(client, device.id));
            }
        }
        effects.push(Effect::Send(client, Reply::EnableSeat));
        c.state = State::Active;
        self.active = Some(client);
    }

    fn disable(&mut self, client: ClientId, effects: &mut Vec<Effect>) {
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        if c.state != State::Active {
            return;
        }
        for device in &mut c.devices {
            if device.active {
                device.active = false;
                effects.push(Effect::Deactivate(client, device.id));
            }
        }
        c.state = State::PendingDisable;
        effects.push(Effect::Send(client, Reply::DisableSeat));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sends(effects: &[Effect], client: ClientId) -> Vec<Reply> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send(c, reply) if *c == client => Some(reply.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn paths_are_the_devices_a_compositor_opens() {
        assert_eq!(device_kind("/dev/dri/card0"), Some(DeviceKind::Drm));
        assert_eq!(device_kind("/dev/dri/renderD128"), Some(DeviceKind::Drm));
        assert_eq!(device_kind("/dev/input/event4"), Some(DeviceKind::Evdev));
        assert_eq!(device_kind("/dev/hidraw0"), Some(DeviceKind::Hidraw));
        for path in [
            "/dev/sda",
            "/dev/input/mice",
            "/dev/dri/",
            "/dev/dri/by-path/x",
            "/etc/shadow",
        ] {
            assert_eq!(device_kind(path), None, "{path}");
        }
    }

    #[test]
    fn the_first_client_gets_the_seat_and_its_vt() {
        let mut seat = Seat::new();
        let effects = seat.open_seat(1, 1);
        assert_eq!(
            effects,
            vec![
                Effect::Send(1, Reply::SeatOpened("seat0".into())),
                Effect::OpenVt(1),
                Effect::Send(1, Reply::EnableSeat),
            ]
        );
        assert_eq!(seat.active(), Some(1));
        // A second one on the same VT, while the first holds it: busy.
        assert_eq!(sends(&seat.open_seat(2, 1), 2), vec![Reply::Error(EBUSY)]);
    }

    #[test]
    fn devices_only_for_the_active_client_and_counted() {
        let mut seat = Seat::new();
        seat.open_seat(1, 1);
        assert_eq!(seat.open_device(1, "/etc/passwd"), Err(ENOENT));
        assert_eq!(seat.open_device(9, "/dev/dri/card0"), Err(EINVAL));
        let Ok(Open::New { id, kind }) = seat.open_device(1, "/dev/dri/card0") else {
            panic!("a new device");
        };
        seat.opened(1, id, "/dev/dri/card0", kind);
        assert_eq!(seat.open_device(1, "/dev/dri/card0"), Ok(Open::Again(id)));
        // Two references: the first close keeps it, the second closes it.
        assert_eq!(
            seat.close_device(1, id),
            vec![Effect::Send(1, Reply::DeviceClosed)]
        );
        assert_eq!(
            seat.close_device(1, id),
            vec![Effect::Close(1, id), Effect::Send(1, Reply::DeviceClosed)]
        );
        assert_eq!(
            sends(&seat.close_device(1, id), 1),
            vec![Reply::Error(EBADF)]
        );
    }

    #[test]
    fn a_vt_switch_takes_the_devices_and_gives_them_back() {
        let mut seat = Seat::new();
        seat.open_seat(1, 1);
        let Ok(Open::New { id: card, kind }) = seat.open_device(1, "/dev/dri/card0") else {
            panic!();
        };
        seat.opened(1, card, "/dev/dri/card0", kind);
        let Ok(Open::New { id: keys, kind }) = seat.open_device(1, "/dev/input/event0") else {
            panic!();
        };
        seat.opened(1, keys, "/dev/input/event0", kind);

        // Away: both taken, the client told, the kernel let go on.
        assert_eq!(
            seat.vt_release(),
            vec![
                Effect::Deactivate(1, card),
                Effect::Deactivate(1, keys),
                Effect::Send(1, Reply::DisableSeat),
                Effect::AckVt { release: true },
            ]
        );
        // Disabled, it may open nothing.
        assert_eq!(seat.open_device(1, "/dev/input/event1"), Err(EPERM));
        assert_eq!(sends(&seat.disable_acked(1), 1), vec![Reply::SeatDisabled]);
        assert_eq!(seat.active(), None);
        // Another VT, no client there: nothing to enable.
        assert_eq!(seat.vt_acquire(2), vec![Effect::AckVt { release: false }]);
        // Back: the card's master again; the keyboard was revoked for good.
        seat.vt_release();
        assert_eq!(
            seat.vt_acquire(1),
            vec![
                Effect::AckVt { release: false },
                Effect::OpenVt(1),
                Effect::Activate(1, card),
                Effect::Send(1, Reply::EnableSeat),
            ]
        );
    }

    #[test]
    fn switching_vt_asks_the_kernel_and_two_clients_take_turns() {
        let mut seat = Seat::new();
        seat.open_seat(1, 1);
        assert_eq!(
            seat.switch_session(1, 2),
            vec![
                Effect::SwitchVt { from: 1, to: 2 },
                Effect::Send(1, Reply::SessionSwitched)
            ]
        );
        assert_eq!(
            sends(&seat.switch_session(1, 0), 1),
            vec![Reply::Error(EINVAL)]
        );
        seat.vt_release();
        seat.disable_acked(1);
        seat.vt_acquire(2);
        // On VT 2 a second compositor joins and gets the seat.
        let effects = seat.open_seat(2, 2);
        assert!(effects.contains(&Effect::Send(2, Reply::EnableSeat)));
        assert_eq!(seat.active(), Some(2));
        // An inactive client may not switch.
        assert_eq!(
            sends(&seat.switch_session(1, 2), 1),
            vec![Reply::Error(EPERM)]
        );
    }

    #[test]
    fn a_client_that_goes_closes_its_devices_and_its_vt() {
        let mut seat = Seat::new();
        seat.open_seat(1, 1);
        let Ok(Open::New { id, kind }) = seat.open_device(1, "/dev/dri/card0") else {
            panic!();
        };
        seat.opened(1, id, "/dev/dri/card0", kind);
        assert_eq!(
            seat.close_seat(1, false),
            vec![Effect::Close(1, id), Effect::CloseVt(1)]
        );
        assert_eq!(seat.active(), None);
        // The next compositor on that VT — the session after the greeter.
        let effects = seat.open_seat(2, 1);
        assert!(effects.contains(&Effect::Send(2, Reply::EnableSeat)));
        assert_eq!(
            seat.close_seat(2, true).last(),
            Some(&Effect::Send(2, Reply::SeatClosed))
        );
    }
}
