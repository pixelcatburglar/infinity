//! Descriptor-backed protobuf text for adaptation notifications. The embedded
//! gateway pool supplies the descriptors; arbitrary message names are not exposed.
//! Mission and node mutations are not implemented by this codec.

use anyhow::{bail, ensure, Context, Result};
use prost_reflect::{DynamicMessage, Kind, MessageDescriptor, Value};
use crate::gateway::message::pool;

pub(crate) mod gateway;

#[derive(Clone, Copy)]
pub(crate) enum Message { Tree, Mission, User, Exchange }
impl Message {
    fn name(self) -> &'static str {
        match self {
            Self::Tree => "eve.shipadaption.Responsed",
            Self::Mission => "eve.shipadaption.mission.Responsed",
            Self::User => "eve.shipadaption.user.Responsed",
            Self::Exchange => "eve.shipadaption.user.ChipExchangeSuccessed",
        }
    }
}

pub(crate) struct Identity { pub user: i64, pub character: i64 }
pub(crate) struct Chip { pub type_id: i32, pub quantity: u32 }
pub(crate) enum Response<'a> {
    Tree { identity: Identity, tree: u32 },
    Mission { identity: Identity },
    User { identity: Identity, chips: &'a [Chip] },
    Exchange { identity: Identity, corporation: i64 },
}

fn descriptor(message: Message) -> Result<MessageDescriptor> {
    pool().get_message_by_name(message.name()).with_context(|| format!("missing descriptor {}", message.name()))
}
fn field(desc: &MessageDescriptor, name: &str, number: u32, repeated: bool) -> Result<prost_reflect::FieldDescriptor> {
    let field = desc.get_field_by_name(name).with_context(|| format!("missing {}.{name}", desc.full_name()))?;
    ensure!(field.number() == number && field.is_list() == repeated, "incompatible descriptor {}.{name}", desc.full_name());
    Ok(field)
}
fn message_field(desc: &MessageDescriptor, name: &str, number: u32, repeated: bool, expected: Option<&str>) -> Result<MessageDescriptor> {
    let f = field(desc, name, number, repeated)?;
    let Kind::Message(child) = f.kind() else { bail!("{}.{name} is not a message", desc.full_name()) };
    if let Some(expected) = expected { ensure!(child.full_name() == expected, "unexpected message type {}", child.full_name()); }
    Ok(child)
}
fn identifier_descriptor(desc: &MessageDescriptor, expected: Kind) -> Result<()> {
    let f = field(desc, "sequential", 1, false)?;
    ensure!(f.kind() == expected, "identifier sequential has unexpected type");
    Ok(())
}
fn compound_identifier(desc: &MessageDescriptor) -> Result<()> {
    for (name, number, expected, scalar) in [
        ("user", 1, "eve.user.Identifier", Kind::Sint32),
        ("character", 2, "eve.character.Identifier", Kind::Uint32),
        ("node", 3, "eve.shipadaption.node.Identifier", Kind::Uint32),
    ] {
        identifier_descriptor(&message_field(desc, name, number, false, Some(expected))?, scalar)?;
    }
    Ok(())
}
fn scalar(desc: &MessageDescriptor, name: &str, number: u32, expected: Kind) -> Result<()> {
    ensure!(field(desc, name, number, false)?.kind() == expected, "unexpected scalar {}.{name}", desc.full_name());
    Ok(())
}
fn identifier(parent: &MessageDescriptor, name: &str, id: i64) -> Result<DynamicMessage> {
    ensure!(id > 0, "identifier must be positive");
    let f = parent.get_field_by_name(name).context("missing identifier field")?;
    let Kind::Message(desc) = f.kind() else { bail!("identifier is not a message") };
    let sequential = desc.get_field_by_name("sequential").context("missing sequential")?;
    let value = match sequential.kind() {
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => Value::I32(i32::try_from(id)?),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => Value::I64(id),
        Kind::Uint32 | Kind::Fixed32 => Value::U32(u32::try_from(id)?),
        Kind::Uint64 | Kind::Fixed64 => Value::U64(u64::try_from(id)?),
        _ => bail!("unsupported sequential kind"),
    };
    let mut result = DynamicMessage::new(desc);
    result.try_set_field(&sequential, value)?;
    Ok(result)
}
fn set_identifier(message: &mut DynamicMessage, desc: &MessageDescriptor, name: &str, id: i64) -> Result<()> {
    message.try_set_field_by_name(name, Value::Message(identifier(desc, name, id)?))?;
    Ok(())
}
/// Validates the exact response descriptors when the package is built.
pub(crate) fn validate() -> Result<()> {
    for kind in [Message::Tree, Message::Mission, Message::User, Message::Exchange] {
        let desc = descriptor(kind)?;
        for (name, number, expected, scalar) in [("user", 1, "eve.user.Identifier", Kind::Sint32), ("character", 2, "eve.character.Identifier", Kind::Uint32)] {
            identifier_descriptor(&message_field(&desc, name, number, false, Some(expected))?, scalar)?;
        }
        match kind {
            Message::Tree => {
                let tree = message_field(&desc, "tree", 3, false, Some("eve.shipadaption.tree.Identifier"))?;
                ensure!(matches!(field(&tree, "sequential", 1, false)?.kind(), Kind::Uint32), "tree identifier must be uint32");
                let node = message_field(&desc, "unlock_nodes", 4, true, Some("eve.shipadaption.node.user.UserNode"))?;
                compound_identifier(&message_field(&node, "id", 1, false, Some("eve.shipadaption.node.user.Identifier"))?)?;
                let attributes = message_field(&node, "attributes", 2, false, Some("eve.shipadaption.node.user.Attributes"))?;
                scalar(&attributes, "level", 1, Kind::Uint32)?;
                let consumed = message_field(&desc, "consumed_chips", 5, true, Some("eve.shipadaption.Responsed.ConsumedChip"))?;
                identifier_descriptor(&message_field(&consumed, "type", 1, false, Some("eve.inventory.genericitemtype.Identifier"))?, Kind::Uint64)?;
                scalar(&consumed, "consumed_quantity", 2, Kind::Uint32)?;
                scalar(&consumed, "last_quantity", 3, Kind::Uint32)?;
            }
            Message::Mission => {
                let mission = message_field(&desc, "missions", 3, true, Some("eve.shipadaption.mission.UserMission"))?;
                compound_identifier(&message_field(&mission, "id", 1, false, Some("eve.shipadaption.mission.Identifier"))?)?;
                let attributes = message_field(&mission, "attributes", 2, false, Some("eve.shipadaption.mission.Attributes"))?;
                scalar(&attributes, "unlocked", 1, Kind::Bool)?;
                scalar(&attributes, "mission_process", 2, Kind::Uint32)?;
                compound_identifier(&message_field(&desc, "active_mission", 4, false, Some("eve.shipadaption.mission.Identifier"))?)?;
            }
            Message::User => {
                let chip = message_field(&desc, "chips", 3, true, Some("eve.shipadaption.user.Chip"))?;
                identifier_descriptor(&message_field(&chip, "type", 1, false, Some("eve.inventory.genericitemtype.Identifier"))?, Kind::Uint64)?;
                ensure!(matches!(field(&chip, "quantity", 2, false)?.kind(), Kind::Uint32), "chip quantity must be uint32");
            }
            Message::Exchange => { identifier_descriptor(&message_field(&desc, "corporation", 3, false, Some("eve.corporation.Identifier"))?, Kind::Uint32)?; }
        }
    }
    Ok(())
}
pub(crate) fn encode_content_text(response: Response<'_>) -> Result<String> {
    let (kind, identity) = match &response {
        Response::Tree { identity, .. } => (Message::Tree, identity),
        Response::Mission { identity } => (Message::Mission, identity),
        Response::User { identity, .. } => (Message::User, identity),
        Response::Exchange { identity, .. } => (Message::Exchange, identity),
    };
    let desc = descriptor(kind)?;
    let mut result = DynamicMessage::new(desc.clone());
    set_identifier(&mut result, &desc, "user", identity.user)?;
    set_identifier(&mut result, &desc, "character", identity.character)?;
    match response {
        Response::Tree { tree, .. } => set_identifier(&mut result, &desc, "tree", i64::from(tree))?,
        Response::Mission { .. } => (),
        Response::User { chips, .. } => {
            let chip_desc = message_field(&desc, "chips", 3, true, Some("eve.shipadaption.user.Chip"))?;
            let mut values = Vec::with_capacity(chips.len());
            for chip in chips {
                let mut value = DynamicMessage::new(chip_desc.clone());
                set_identifier(&mut value, &chip_desc, "type", i64::from(chip.type_id))?;
                value.try_set_field_by_name("quantity", Value::U32(chip.quantity))?;
                values.push(Value::Message(value));
            }
            result.try_set_field_by_name("chips", Value::List(values))?;
        }
        Response::Exchange { corporation, .. } => set_identifier(&mut result, &desc, "corporation", corporation)?,
    }
    Ok(result.to_text_format())
}

#[cfg(test)]
mod tests;
