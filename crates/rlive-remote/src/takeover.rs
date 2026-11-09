//! Absolute controllers and values they have lost touch with (manual pp. 868-869).

/// What a physical control does when it no longer agrees with its parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TakeoverMode {
    /// The parameter jumps to the control.
    None,
    /// Nothing happens until the control reaches the parameter.
    #[default]
    PickUp,
    /// The parameter moves towards the control, meeting it at the end of the control's travel.
    ValueScaling,
}

impl TakeoverMode {
    pub const ALL: [Self; 3] = [Self::None, Self::PickUp, Self::ValueScaling];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::PickUp => "Pick-Up",
            Self::ValueScaling => "Value Scaling",
        }
    }

    /// As stored in an atomic.
    pub fn to_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::PickUp => 1,
            Self::ValueScaling => 2,
        }
    }

    pub fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::None,
            2 => Self::ValueScaling,
            _ => Self::PickUp,
        }
    }
}

/// How close counts as having reached the parameter.
const MEET: f64 = 1.0 / 127.0 / 2.0;

/// One control's relationship with its parameter. Everything is 0 to 1 over the mapping's range.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Takeover {
    /// Where the control last was. `None` for a control that hasn't moved yet.
    pub last_control: Option<f64>,
    /// The parameter is following the control 1:1.
    pub engaged: bool,
    /// What we last sent, to notice something else moving the parameter.
    pub last_sent: Option<f64>,
}

impl Takeover {
    /// The control moved to `control` with the parameter at `param`. Returns the parameter's
    /// new value, if it should have one.
    pub fn apply(&mut self, mode: TakeoverMode, control: f64, param: f64) -> Option<f64> {
        // Moved by something else since we last sent it: the control has lost touch.
        if self.last_sent.is_some_and(|s| (s - param).abs() > MEET) {
            self.engaged = false;
        }
        let previous = self.last_control.replace(control);
        let reached = (control - param).abs() <= MEET
            || previous.is_some_and(|p| (p - param) * (control - param) <= 0.0);
        if reached {
            self.engaged = true;
        }
        let out = match mode {
            TakeoverMode::None => Some(control),
            _ if self.engaged => Some(control),
            TakeoverMode::PickUp => None,
            TakeoverMode::ValueScaling => previous.map(|p| converge(param, p, control)),
        };
        if out.is_some() {
            self.last_sent = out;
        }
        out
    }
}

/// How far the parameter moves when the control goes from `from` to `to`: the same fraction of
/// the way to the end as the control just covered, so both get to the end together.
fn converge(param: f64, from: f64, to: f64) -> f64 {
    if to > from {
        if from >= 1.0 {
            param
        } else {
            (param + (to - from) * (1.0 - param) / (1.0 - from)).clamp(0.0, 1.0)
        }
    } else if to < from {
        if from <= 0.0 {
            param
        } else {
            (param - (from - to) * param / from).clamp(0.0, 1.0)
        }
    } else {
        param
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TakeoverMode as M;

    #[test]
    fn none_jumps_straight_to_the_control() {
        let mut t = Takeover::default();
        assert_eq!(t.apply(M::None, 0.9, 0.1), Some(0.9));
        assert_eq!(t.apply(M::None, 0.2, 0.9), Some(0.2));
    }

    #[test]
    fn pick_up_waits_for_the_control_to_reach_the_parameter() {
        let mut t = Takeover::default();
        assert_eq!(t.apply(M::PickUp, 0.1, 0.5), None);
        assert_eq!(t.apply(M::PickUp, 0.3, 0.5), None);
        // Crossed it on the way: from here on it's 1:1.
        assert_eq!(t.apply(M::PickUp, 0.6, 0.5), Some(0.6));
        assert_eq!(t.apply(M::PickUp, 0.4, 0.6), Some(0.4));
    }

    #[test]
    fn pick_up_lets_go_when_something_else_moves_the_parameter() {
        let mut t = Takeover::default();
        assert_eq!(t.apply(M::PickUp, 0.5, 0.5), Some(0.5));
        // A mouse moved it, say, or a bank switch: the control has to find it again.
        assert_eq!(t.apply(M::PickUp, 0.6, 0.2), None);
        assert_eq!(t.apply(M::PickUp, 0.1, 0.2), Some(0.1), "crossed");
    }

    #[test]
    fn value_scaling_converges_at_the_end_of_travel() {
        let mut t = Takeover::default();
        // The first move only says where the control is.
        assert_eq!(t.apply(M::ValueScaling, 0.8, 0.2), None);
        // Going down from 0.8 with the parameter at 0.2: both reach zero together.
        let mut param = 0.2;
        let mut control = 0.8;
        while control > 0.2 {
            let next = control - 0.1;
            param = t.apply(M::ValueScaling, next, param).unwrap();
            control = next;
            assert!(param > 0.0 && param < 0.2);
        }
        let last = t.apply(M::ValueScaling, 0.0, param).unwrap();
        assert!(last.abs() < 1e-9, "both end at zero, not {last}");
    }

    #[test]
    fn value_scaling_goes_1_to_1_once_they_meet() {
        let mut t = Takeover::default();
        t.apply(M::ValueScaling, 0.4, 0.5);
        // Control passes the parameter, so it has met it.
        assert_eq!(t.apply(M::ValueScaling, 0.6, 0.5), Some(0.6));
        assert_eq!(t.apply(M::ValueScaling, 0.7, 0.6), Some(0.7));
    }
}
