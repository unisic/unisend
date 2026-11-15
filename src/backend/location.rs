//! Sending the user's position: the `m.location` message and what each bridge makes of it.

use matrix_sdk::ruma::MilliSecondsSinceUnixEpoch;
use matrix_sdk::ruma::events::room::message::{LocationMessageEventContent, MessageType, RoomMessageEventContentWithoutRelation};
use crate::i18n::t;

/// `geo:<lat>,<lon>` with the accuracy as RFC 5870's uncertainty parameter. Never an altitude: the
/// Signal and WhatsApp bridges accept exactly two numbers before the `;`
/// (mautrix/signal pkg/msgconv/from-matrix.go `parseGeoURI`).
pub fn geo_uri(lat: f64, lon: f64, accuracy: Option<f64>) -> String {
    match accuracy.filter(|a| a.is_finite() && *a > 0.) {
        Some(a) => format!("geo:{lat:.6},{lon:.6};u={}", a.round()),
        None => format!("geo:{lat:.6},{lon:.6}"),
    }
}

/// The message: `body` and `geo_uri` for every client and bridge, plus the MSC3488 fields. The
/// WhatsApp bridge shows `body` as the location's comment, so it holds only the coordinates.
pub fn content(lat: f64, lon: f64, accuracy: Option<f64>) -> RoomMessageEventContentWithoutRelation {
    let location = LocationMessageEventContent::new(format!("{lat:.5}, {lon:.5}"), geo_uri(lat, lon, accuracy)).with_ts(MilliSecondsSinceUnixEpoch::now());
    RoomMessageEventContentWithoutRelation::new(MessageType::Location(location))
}

/// What the room's network does with a location, told before sending; `None` when it takes one
/// as a map pin. `block` is a network that cannot take it at all.
pub struct Support {
    pub note: &'static str,
    pub block: bool,
}

/// From the bridge's display name (the `m.bridge` protocol), for a bridge that publishes no
/// `com.beeper.room_features`; where it does, its `location_message` decides. mautrix-whatsapp sends a location
/// message (whatsapp pkg/msgconv/from-matrix.go `case event.MsgLocation`); mautrix-signal sends the
/// coordinates as a Google Maps link in the text (signal from-matrix.go, `location_format`);
/// mautrix-meta has no Instagram support (igconv/from-matrix.go falls to the unsupported default) and
/// for Messenger only the encrypted path sends a card, without a map (meta to-whatsapp.go),
/// while the other path returns unsupported (meta from-matrix.go `// TODO implement`).
pub fn support(network: &str) -> Option<Support> {
    let n = network.to_ascii_lowercase();
    if n.contains("instagram") {
        Some(Support { note: t!("Instagram cannot receive locations, so this one would not arrive."), block: true })
    } else if n.contains("messenger") {
        Some(Support { note: t!("Messenger may not accept locations: in an encrypted chat it shows a plain shared-location card with no map, elsewhere it can fail."), block: false })
    } else if n.contains("signal") {
        Some(Support { note: t!("Signal has no location messages, so it receives a map link as text."), block: false })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_has_two_numbers_and_uncertainty() {
        assert_eq!(geo_uri(52.52008, 13.40495, Some(12.4)), "geo:52.520080,13.404950;u=12");
        assert_eq!(geo_uri(-1.5, 2., None), "geo:-1.500000,2.000000");
        assert_eq!(geo_uri(1., 2., Some(f64::NAN)), "geo:1.000000,2.000000");
    }

    #[test]
    fn message_carries_both_shapes_and_reads_back() {
        let json = serde_json::to_value(content(52.52008, 13.40495, Some(8.))).unwrap();
        assert_eq!(json["msgtype"], "m.location");
        assert_eq!(json["geo_uri"], "geo:52.520080,13.404950;u=8");
        assert_eq!(json["body"], "52.52008, 13.40495");
        assert_eq!(json["org.matrix.msc3488.location"]["uri"], "geo:52.520080,13.404950;u=8");
        assert!(json["org.matrix.msc3488.ts"].is_number());
        let place = super::super::Place::parse(json["geo_uri"].as_str().unwrap(), json["body"].as_str().unwrap()).unwrap();
        assert!(place.description.is_empty() && (place.lat - 52.52008).abs() < 1e-9);
    }

    #[test]
    fn networks() {
        assert!(support("WhatsApp").is_none());
        assert!(support("Instagram").unwrap().block);
        assert!(!support("Facebook Messenger").unwrap().block);
        assert!(support("Signal").is_some());
    }
}
