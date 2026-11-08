use serde::{Deserialize, Serialize};

/// How a parameter should be presented and quantised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamKind {
    /// Continuous value. `skew` > 1 gives more knob resolution near `min`
    /// (useful for frequencies and times).
    Continuous { skew: f64 },
    /// Integer steps between min and max, optionally with names.
    Stepped { labels: Vec<String> },
    /// On/off switch (0.0 / 1.0).
    Toggle,
}

/// Description of an automatable parameter, in *plain* units (Hz, dB, ms, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: String,
    pub kind: ParamKind,
}

impl ParamInfo {
    pub fn continuous(id: u32, name: &str, min: f64, max: f64, default: f64, unit: &str) -> Self {
        Self {
            id,
            name: name.into(),
            min,
            max,
            default,
            unit: unit.into(),
            kind: ParamKind::Continuous { skew: 1.0 },
        }
    }

    /// Continuous parameter with logarithmic-feeling response (frequencies, times).
    pub fn skewed(
        id: u32,
        name: &str,
        min: f64,
        max: f64,
        default: f64,
        unit: &str,
        skew: f64,
    ) -> Self {
        Self {
            kind: ParamKind::Continuous { skew },
            ..Self::continuous(id, name, min, max, default, unit)
        }
    }

    pub fn stepped(id: u32, name: &str, labels: &[&str], default: usize) -> Self {
        Self {
            id,
            name: name.into(),
            min: 0.0,
            max: (labels.len().max(1) - 1) as f64,
            default: default as f64,
            unit: String::new(),
            kind: ParamKind::Stepped {
                labels: labels.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    pub fn toggle(id: u32, name: &str, default: bool) -> Self {
        Self {
            id,
            name: name.into(),
            min: 0.0,
            max: 1.0,
            default: if default { 1.0 } else { 0.0 },
            unit: String::new(),
            kind: ParamKind::Toggle,
        }
    }

    /// Map a plain value to `0..=1` knob position.
    pub fn to_normalized(&self, value: f64) -> f64 {
        if self.max <= self.min {
            return 0.0;
        }
        let lin = ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        match self.kind {
            ParamKind::Continuous { skew } if skew != 1.0 => lin.powf(1.0 / skew),
            _ => lin,
        }
    }

    /// Map a `0..=1` knob position back to a plain value (quantised for stepped params).
    pub fn from_normalized(&self, norm: f64) -> f64 {
        let norm = norm.clamp(0.0, 1.0);
        let lin = match self.kind {
            ParamKind::Continuous { skew } if skew != 1.0 => norm.powf(skew),
            _ => norm,
        };
        let v = self.min + lin * (self.max - self.min);
        match self.kind {
            ParamKind::Stepped { .. } | ParamKind::Toggle => v.round(),
            _ => v,
        }
    }

    /// Human readable value, e.g. `"440 Hz"` or `"Saw"`.
    pub fn format(&self, value: f64) -> String {
        match &self.kind {
            ParamKind::Stepped { labels } => labels
                .get(value.round().max(0.0) as usize)
                .cloned()
                .unwrap_or_else(|| format!("{}", value.round())),
            ParamKind::Toggle => if value >= 0.5 { "On" } else { "Off" }.to_string(),
            ParamKind::Continuous { .. } => {
                let abs = value.abs();
                let num = if abs >= 1000.0 && self.unit == "Hz" {
                    return format!("{:.2} kHz", value / 1000.0);
                } else if abs >= 100.0 {
                    format!("{value:.0}")
                } else if abs >= 10.0 {
                    format!("{value:.1}")
                } else {
                    format!("{value:.2}")
                };
                if self.unit.is_empty() {
                    num
                } else {
                    format!("{num} {}", self.unit)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_roundtrip() {
        let p = ParamInfo::skewed(0, "Cutoff", 20.0, 20000.0, 1000.0, "Hz", 3.0);
        for v in [20.0, 100.0, 1000.0, 20000.0] {
            let n = p.to_normalized(v);
            assert!((p.from_normalized(n) - v).abs() / v < 1e-9);
        }
        let s = ParamInfo::stepped(1, "Wave", &["Saw", "Square", "Sine"], 0);
        assert_eq!(s.from_normalized(0.6), 1.0);
        assert_eq!(s.format(2.0), "Sine");
        assert_eq!(p.format(2500.0), "2.50 kHz");
    }
}
