//! Stav uživatele (dostupný, zaneprázdněný, pryč…).

use serde::{Deserialize, Serialize};

/// Dostupnost, jak ji Teams ukazuje barvou tečky.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Availability {
    Available,
    Busy,
    DoNotDisturb,
    BeRightBack,
    Away,
    Offline,
    #[default]
    Unknown,
}

impl Availability {
    /// Hodnota `availability` ze služby přítomnosti (`AvailableIdle` → dostupný, `BusyIdle` → zaneprázdněný).
    pub fn parse(s: &str) -> Self {
        match s {
            "Available" | "AvailableIdle" => Self::Available,
            "Busy" | "BusyIdle" => Self::Busy,
            "DoNotDisturb" => Self::DoNotDisturb,
            "BeRightBack" => Self::BeRightBack,
            "Away" => Self::Away,
            "Offline" => Self::Offline,
            _ => Self::Unknown,
        }
    }
}

/// Stav jednoho uživatele.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Presence {
    pub availability: Availability,
    /// Podrobnější činnost (`InACall`, `InAMeeting`, `Presenting`, `OutOfOffice`…), pokud se liší.
    pub activity: Option<String>,
    /// Zapnutá automatická odpověď „mimo kancelář“.
    pub out_of_office: bool,
    /// Ručně nastavený stav, pokud platí (má přednost před automatickým).
    #[serde(default)]
    pub forced: Option<Availability>,
}

impl Presence {
    /// Popisek stavu pro UI.
    pub fn label(&self) -> &'static str {
        match self.activity.as_deref() {
            Some("InACall") => return "V hovoru",
            Some("InAConferenceCall") => return "V konferenčním hovoru",
            Some("InAMeeting") => return "Na schůzce",
            Some("Presenting") => return "Prezentuje",
            Some("Focusing") => return "Soustředí se",
            Some("OutOfOffice") => return "Mimo kancelář",
            Some("OffWork") => return "Mimo pracovní dobu",
            _ => {}
        }
        match self.availability {
            Availability::Available => "Dostupný",
            Availability::Busy => "Zaneprázdněný",
            Availability::DoNotDisturb => "Nerušit",
            Availability::BeRightBack => "Hned jsem zpět",
            Availability::Away => "Pryč",
            Availability::Offline => "Offline",
            Availability::Unknown => "Stav neznámý",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_labels() {
        assert_eq!(
            Availability::parse("AvailableIdle"),
            Availability::Available
        );
        assert_eq!(Availability::parse("BusyIdle"), Availability::Busy);
        assert_eq!(
            Availability::parse("PresenceUnknown"),
            Availability::Unknown
        );
        let call = Presence {
            availability: Availability::Busy,
            activity: Some("InACall".into()),
            ..Default::default()
        };
        assert_eq!(call.label(), "V hovoru");
        assert_eq!(Presence::default().label(), "Stav neznámý");
        let away = Presence {
            availability: Availability::Away,
            activity: Some("Away".into()),
            ..Default::default()
        };
        assert_eq!(away.label(), "Pryč");
    }
}
