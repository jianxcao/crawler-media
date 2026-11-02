use std::{fmt, str::FromStr};
use uuid::Uuid;

macro_rules! entity_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(Uuid);

        impl serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct IdVisitor;
                impl<'de> serde::de::Visitor<'de> for IdVisitor {
                    type Value = Uuid;
                    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                        formatter.write_str("a valid UUID string")
                    }
                    fn visit_str<E>(self, value: &str) -> Result<Uuid, E>
                    where
                        E: serde::de::Error,
                    {
                        Uuid::parse_str(value).map_err(serde::de::Error::custom)
                    }
                }
                let uuid = deserializer.deserialize_str(IdVisitor)?;
                Ok(Self(uuid))
            }
        }

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

entity_id!(MediaId);
entity_id!(SiteId);
entity_id!(UserId);
entity_id!(SubscribeId);
entity_id!(FilterId);
entity_id!(LedgerId);
entity_id!(DownloaderId);
entity_id!(JobId);
entity_id!(JobDefId);
entity_id!(CollectionId);
entity_id!(LibraryId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_entity_ids_are_fresh_v4_uuids() {
        let ids = [
            MediaId::default().as_uuid(),
            SiteId::default().as_uuid(),
            UserId::default().as_uuid(),
            SubscribeId::default().as_uuid(),
            FilterId::default().as_uuid(),
            LedgerId::default().as_uuid(),
            DownloaderId::default().as_uuid(),
            JobId::default().as_uuid(),
            JobDefId::default().as_uuid(),
            CollectionId::default().as_uuid(),
            LibraryId::default().as_uuid(),
        ];
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(id.get_version_num(), 4);
            assert!(!ids[..index].contains(id));
        }
    }
}
