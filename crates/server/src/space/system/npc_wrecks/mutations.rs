//! Actor-only content mutations for transient NPC wrecks and their loose-loot
//! containers. Inventory rows and source notices retain the existing invCache
//! representation; state and static type inputs come from the owning actor.
//! These mutations do not persist items or restore loot after actor exit.

use super::*;
use crate::services::inventory::{hold_storage, is_internal_modifier_flag};
use crate::services::inventory::movement::{classify_move_flag, item_destination_flag};
use crate::services::inventory::notifications::{item_change_dict, removed_payload};
use infinity_core::consts::{IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, IX_STACK_SIZE, MAX_STACK_QUANTITY};

impl System {
    /// Committed take: the whole original ID leaves, or the original keeps its
    /// exact remainder; the new slice's destination row is the movement
    /// outbox's job, never this source notification.
    pub(super) fn npc_wreck_apply_take(&mut self, claim: &Arc<NpcWreckClaim>) {
        let root = claim.root;
        let NpcWreckOperation::Take { item, .. } = &claim.operation else { return };
        let Some(taken) = claim.taken.clone() else { return };
        let Some((type_id, flag)) = self.npc_wrecks.roots.get(&root)
            .map(|entry| (entry.item.type_id, entry.item.flag_id)) else { return };
        let Some(entry) = self.npc_wrecks.roots.get_mut(&root) else { return };
        let was_empty = entry.items.is_empty();
        let payload = if taken.item_id == *item {
            match entry.items.iter().position(|i| i.item_id == *item) {
                Some(index) => {
                    let removed = entry.items.remove(index);
                    self.npc_wrecks.child_to_root.remove(item);
                    removed_payload(removed)
                }
                None => return,
            }
        } else {
            let Some(source) = entry.items.iter_mut().find(|i| i.item_id == *item) else { return };
            let old = source.quantity;
            source.quantity -= taken.quantity;
            (source.row(), item_change_dict([(IX_STACK_SIZE, Value::Int(old))]))
        };
        entry.revision = entry.revision.saturating_add(1);
        self.npc_wreck_emit(root, type_id, flag, vec![payload]);
        self.npc_wreck_refresh_is_empty(root, was_empty);
    }

    /// Committed salvage: claim released first, then the old root ball leaves
    /// through the normal exit (tractors and followers stop), then the
    /// replacement can installs with the original children, and the captured
    /// source audience sees each item's location change.
    pub(super) fn npc_wreck_apply_salvage(&mut self, claim: &Arc<NpcWreckClaim>) {
        let root = claim.root;
        let Some(entry) = self.npc_wrecks.roots.remove(&root) else { return };
        for item in &entry.items { self.npc_wrecks.child_to_root.remove(&item.item_id); }
        let audience = self.npc_wreck_audience(root, entry.item.type_id, entry.item.flag_id);
        self.remove_ball(root);
        let Some(ball) = claim.replacement.clone() else { return };
        let new_root = ball.id;
        let payloads: Vec<(Value, Value)> = entry.items.iter().map(|item| {
            let mut moved = item.clone();
            moved.location_id = new_root;
            (moved.row(), item_change_dict([(IX_LOCATION_ID, Value::Int(root))]))
        }).collect();
        self.install_npc_root(NpcWreckKind::LooseLootContainer, ball,
            entry.items.into_iter().map(|mut item| { item.location_id = new_root; item }).collect(), None);
        for character in audience {
            for (row, change) in &payloads {
                self.notify_pilot(character, "OnItemChange", vec![row.clone(), change.clone(), Value::None]);
            }
        }
    }

    /// Committed delete: naming the root removes the whole root; otherwise the
    /// named plain children leave with the standard removal payload.
    pub(super) fn npc_wreck_apply_delete(&mut self, root: i64, items: &[i64]) {
        if items.contains(&root) {
            self.npc_wrecks.drop_root(root);
            self.remove_ball(root);
            return;
        }
        let Some((type_id, flag)) = self.npc_wrecks.roots.get(&root)
            .map(|entry| (entry.item.type_id, entry.item.flag_id)) else { return };
        let Some(entry) = self.npc_wrecks.roots.get_mut(&root) else { return };
        let was_empty = entry.items.is_empty();
        let mut payloads = Vec::new();
        for &id in items {
            if let Some(index) = entry.items.iter().position(|item| item.item_id == id) {
                let removed = entry.items.remove(index);
                self.npc_wrecks.child_to_root.remove(&id);
                payloads.push(removed_payload(removed));
            }
        }
        entry.revision = entry.revision.saturating_add(1);
        self.npc_wreck_emit(root, type_id, flag, payloads);
        self.npc_wreck_refresh_is_empty(root, was_empty);
    }

    /// Pure in-actor content change between turns: full admission, original
    /// owner rules, checked arithmetic and atomic apply. No claim is taken;
    /// a claimed root is simply busy.
    pub(crate) fn mutate_npc_loot(&mut self, identity: &Identity, expected: &NpcWreckIdentity,
        mutation: NpcLootMutation) -> Result<Option<i64>, CallError> {
        let root = expected.root;
        {
            let Some(entry) = self.npc_wrecks.roots.get(&root) else { return Err(denied()) };
            if entry.claim.is_some() || entry.ball_departed { return Err(busy()) }
            if expected.revision != entry.revision || expected.token != entry.token
                || !Arc::ptr_eq(&expected.incarnation, &self.npc_wrecks.incarnation)
                || !self.ball_token_current(expected.token) { return Err(denied()) }
            if !self.npc_wreck_admit(identity, entry) { return Err(denied()) }
        }
        match mutation {
            NpcLootMutation::Split { item, quantity } =>
                self.npc_wreck_split(identity, root, item, quantity, None),
            NpcLootMutation::Quantities(changes) => {
                {
                    let Some(entry) = self.npc_wrecks.roots.get(&root) else { return Err(denied()) };
                    let (mut before, mut after) = (0i128, 0i128);
                    for (&id, change) in &changes {
                        let Some(item) = entry.items.iter().find(|i| i.item_id == id) else { return Err(denied()) };
                        if item.owner_id != identity.char_id
                            || change.type_id != item.type_id || change.expected_quantity != item.quantity
                            || change.owner_id != item.owner_id || change.location_id != item.location_id
                            || change.flag_id != item.flag_id || change.singleton != item.singleton
                            || change.quantity.is_some_and(|quantity| quantity <= 0) { return Err(denied()) }
                        before += i128::from(item.quantity);
                        after += i128::from(change.quantity.unwrap_or(0));
                    }
                    if before != after {
                        return Err(CallError::Internal(anyhow::anyhow!("npc loot quantity plan is not conserving")));
                    }
                }
                let (type_id, flag) = (self.npc_wrecks.roots[&root].item.type_id, self.npc_wrecks.roots[&root].item.flag_id);
                let was_empty = self.npc_wrecks.roots[&root].items.is_empty();
                let mut payloads = Vec::with_capacity(changes.len());
                {
                    let entry = self.npc_wrecks.roots.get_mut(&root).expect("root checked");
                    for (&id, change) in &changes {
                        match change.quantity {
                            Some(quantity) => {
                                let item = entry.items.iter_mut().find(|i| i.item_id == id).expect("verified");
                                let old = item.quantity;
                                item.quantity = quantity;
                                payloads.push((item.row(), item_change_dict([(IX_STACK_SIZE, Value::Int(old))])));
                            }
                            None => {
                                let index = entry.items.iter().position(|i| i.item_id == id).expect("verified");
                                let removed = entry.items.remove(index);
                                self.npc_wrecks.child_to_root.remove(&id);
                                payloads.push(removed_payload(removed));
                            }
                        }
                    }
                    entry.revision = entry.revision.saturating_add(1);
                }
                self.npc_wreck_emit(root, type_id, flag, payloads);
                self.npc_wreck_refresh_is_empty(root, was_empty);
                Ok(None)
            }
            NpcLootMutation::Move { item, quantity, destination, flag } =>
                self.npc_wreck_move(identity, expected, item, quantity, destination, flag),
            NpcLootMutation::Rename { name, row, caller } => {
                {
                    let entry = self.npc_wrecks.roots.get(&root).expect("root checked");
                    if entry.kind != NpcWreckKind::LooseLootContainer
                        || entry.item.owner_id != identity.char_id || caller != identity.char_id {
                        return Err(denied());
                    }
                }
                let entry = self.npc_wrecks.roots.get_mut(&root).expect("root checked");
                entry.name = Some(name.clone());
                entry.item.custom_info = Some(name.clone());
                entry.revision = entry.revision.saturating_add(1);
                self.set_item_name(root, name, &row, caller);
                Ok(None)
            }
        }
    }

    /// Verified-root in-place split shared by the standalone Split mutation
    /// and a same-root partial move: caller-owned item only, the remainder
    /// keeps its flag, and the reserved new slice takes `flag` when given
    /// or the source flag. One revision bump; both rows are notified.
    fn npc_wreck_split(&mut self, identity: &Identity, root: i64, item: i64,
        quantity: i64, flag: Option<i32>) -> Result<Option<i64>, CallError> {
        let Some(index) = self.npc_wrecks.roots.get(&root)
            .and_then(|entry| entry.items.iter().position(|i| i.item_id == item))
        else { return Err(denied()) };
        {
            let entry = &self.npc_wrecks.roots[&root];
            let source = &entry.items[index];
            if source.owner_id != identity.char_id || quantity <= 0 || quantity >= source.quantity {
                return Err(crate::services::notify_error(messages::INSUFFICIENT_ITEM_QUANTITY));
            }
        }
        let Some(ids) = self.npc_wrecks.draw_ids(1) else {
            self.npc_wreck_ensure_ids();
            return Err(busy());
        };
        let (type_id, root_flag) = (self.npc_wrecks.roots[&root].item.type_id, self.npc_wrecks.roots[&root].item.flag_id);
        let was_empty = self.npc_wrecks.roots[&root].items.is_empty();
        let entry = self.npc_wrecks.roots.get_mut(&root).expect("root checked");
        let old = entry.items[index].quantity;
        entry.items[index].quantity = old - quantity;
        let mut created = entry.items[index].clone();
        created.item_id = ids[0];
        created.quantity = quantity;
        let source_row = entry.items[index].clone();
        let source_flag = source_row.flag_id;
        if let Some(flag) = flag { created.flag_id = flag; }
        let created_row = created.row();
        entry.items.push(created);
        entry.items.sort_by_key(|i| i.item_id);
        self.npc_wrecks.child_to_root.insert(ids[0], root);
        entry.revision = entry.revision.saturating_add(1);
        // Match the persistent wire shape per caller: a standalone split
        // reports only the broker's synthetic old location 0; an Add
        // partial reports the real root and the flag it replaced.
        let created_change = match flag {
            Some(_) => item_change_dict([(IX_LOCATION_ID, Value::Int(root)),
                (IX_FLAG, Value::Int(i64::from(source_flag)))]),
            None => item_change_dict([(IX_LOCATION_ID, Value::Int(0))]),
        };
        self.npc_wreck_emit(root, type_id, root_flag, vec![
            (source_row.row(), item_change_dict([(IX_STACK_SIZE, Value::Int(old))])),
            (created_row, created_change),
        ]);
        self.npc_wreck_refresh_is_empty(root, was_empty);
        self.npc_wreck_ensure_ids();
        Ok(Some(ids[0]))
    }

    /// Same-actor transient→transient move: both opaque identities verified,
    /// both roots unclaimed. The source may carry ForeignLoot; only the
    /// caller's own transient root may receive it, so arriving rows are owned
    /// by the caller while the source remainder keeps its original owner.
    /// The flag is re-verified as C's normalized final flag; a same-root
    /// Move is a verified no-op, a whole flag update, or an owner-only split.
    fn npc_wreck_move(&mut self, identity: &Identity, expected: &NpcWreckIdentity,
        item: i64, quantity: i64, destination: NpcWreckIdentity, flag: i32) -> Result<Option<i64>, CallError> {
        let root = expected.root;
        let dest = destination.root;
        if !Arc::ptr_eq(&destination.incarnation, &self.npc_wrecks.incarnation) { return Err(denied()) }
        {
            let Some(entry) = self.npc_wrecks.roots.get(&dest) else { return Err(denied()) };
            if entry.claim.is_some() || entry.ball_departed { return Err(busy()) }
            if destination.revision != entry.revision || destination.token != entry.token
                || !self.ball_token_current(destination.token) { return Err(denied()) }
            if !self.npc_wreck_admit(identity, entry) { return Err(denied()) }
            // ForeignLoot may leave an admitted source, but only the
            // caller's own transient root may receive it.
            if entry.item.owner_id != identity.char_id { return Err(denied()) }
        }
        let (type_id, root_flag, dest_type, dest_flag) = {
            (self.npc_wrecks.roots[&root].item.type_id, self.npc_wrecks.roots[&root].item.flag_id,
                self.npc_wrecks.roots[&dest].item.type_id, self.npc_wrecks.roots[&dest].item.flag_id)
        };
        let source = self.npc_wrecks.roots[&root].items.iter().find(|i| i.item_id == item).cloned()
            .ok_or_else(denied)?;
        if quantity <= 0 || quantity > source.quantity { return Err(denied()) }
        let sde = self.sde().ok_or_else(|| CallError::Internal(anyhow::anyhow!("SDE unavailable")))?;
        // Re-verify the caller-supplied flag as the already-normalized final
        // flag: no corp deliveries or internal modifier, and re-classifying
        // it must be a plain move flag, never fitting or autofit.
        let source_category = sde.types.get(source.type_id)
            .and_then(|t| t.category_id).map(i64::from).unwrap_or(0);
        let dest_category = sde.types.get(dest_type)
            .and_then(|t| t.category_id).map(i64::from).unwrap_or(0);
        if is_internal_modifier_flag(flag)
            || item_destination_flag(dest_category, Some(flag)) != Some(flag)
            || classify_move_flag(source_category, flag) != (flag, false) { return Err(denied()) }
        if dest == root {
            if source.owner_id != identity.char_id { return Err(denied()) }
            if quantity < source.quantity {
                return self.npc_wreck_split(identity, root, item, quantity, Some(flag));
            }
            if flag == source.flag_id { return Ok(None); }
            // Whole same-root flag update: same ID, same owner, one
            // revision bump; the notice carries the flag's old value.
            let entry = self.npc_wrecks.roots.get_mut(&root).expect("root checked");
            let target = entry.items.iter_mut().find(|i| i.item_id == item).expect("verified");
            let old_flag = target.flag_id;
            target.flag_id = flag;
            let row = target.row();
            entry.revision = entry.revision.saturating_add(1);
            self.npc_wreck_emit(root, type_id, root_flag, vec![
                (row, item_change_dict([(IX_LOCATION_ID, Value::Int(root)),
                    (IX_FLAG, Value::Int(i64::from(old_flag)))])),
            ]);
            return Ok(None);
        }
        let merge = self.npc_wrecks.roots[&dest].items.iter()
            .position(|i| i.type_id == source.type_id && i.owner_id == identity.char_id
                && i.flag_id == flag && !i.singleton);
        // Capacity follows the original rules: ordinary containers check room;
        // wrecks keep their original no-limit branch.
        let mut planned = quantity;
        if let Some(index) = merge {
            planned = quantity.min(MAX_STACK_QUANTITY - self.npc_wrecks.roots[&dest].items[index].quantity);
            if planned <= 0 { return Err(denied()) }
        }
        if hold_storage::is_ordinary_container(sde, dest_type) {
            let capacity = hold_storage::static_capacity(sde, dest_type)?;
            let moved = sde.types.get(source.type_id).and_then(|t| t.volume).unwrap_or(0.0) * planned as f64;
            let used: f64 = self.npc_wrecks.roots[&dest].items.iter()
                .map(|i| sde.types.get(i.type_id).and_then(|t| t.volume).unwrap_or(0.0) * i.quantity.max(0) as f64).sum();
            if !capacity.is_finite() || used + moved > capacity {
                return Err(crate::services::user_error(messages::MSG_NOT_ENOUGH_CARGO_SPACE, vec![]));
            }
        }
        let needs_id = merge.is_none() && planned < source.quantity;
        let new_id = if needs_id {
            let Some(ids) = self.npc_wrecks.draw_ids(1) else {
                self.npc_wreck_ensure_ids();
                return Err(busy());
            };
            Some(ids[0])
        } else { None };
        let (was_src_empty, was_dst_empty) = (self.npc_wrecks.roots[&root].items.is_empty(), self.npc_wrecks.roots[&dest].items.is_empty());
        let mut src_payloads = Vec::new();
        let mut dst_payloads = Vec::new();
        {
            // Source first: a whole move removes the old mapping before the
            // destination re-registers the same ID under itself.
            let src_entry = self.npc_wrecks.roots.get_mut(&root).expect("root checked");
            if planned == source.quantity {
                let index = src_entry.items.iter().position(|i| i.item_id == item).expect("verified");
                let removed = src_entry.items.remove(index);
                self.npc_wrecks.child_to_root.remove(&item);
                src_payloads.push(removed_payload(removed));
            } else {
                let src_item = src_entry.items.iter_mut().find(|i| i.item_id == item).expect("verified");
                let old = src_item.quantity;
                src_item.quantity -= planned;
                src_payloads.push((src_item.row(), item_change_dict([(IX_STACK_SIZE, Value::Int(old))])));
            }
            src_entry.revision = src_entry.revision.saturating_add(1);
            let mut moved_row = None;
            let dest_entry = self.npc_wrecks.roots.get_mut(&dest).expect("dest checked");
            if let Some(index) = merge {
                let target = &mut dest_entry.items[index];
                let old = target.quantity;
                target.quantity += planned;
                dst_payloads.push((target.row(), item_change_dict([(IX_STACK_SIZE, Value::Int(old))])));
            } else {
                let mut moved = source.clone();
                moved.item_id = new_id.unwrap_or(source.item_id);
                moved.location_id = dest;
                moved.owner_id = identity.char_id;
                moved.flag_id = flag;
                moved.quantity = planned;
                moved_row = Some(moved.clone());
                dest_entry.items.push(moved);
                dest_entry.items.sort_by_key(|i| i.item_id);
                self.npc_wrecks.child_to_root.insert(new_id.unwrap_or(source.item_id), dest);
            }
            dest_entry.revision = dest_entry.revision.saturating_add(1);
            // The source audience sees only the source delta; the destination
            // audience sees the arriving row, never private cargo contents.
            if let Some(row) = moved_row {
                let mut changes = vec![(IX_LOCATION_ID, Value::Int(root)),
                    (IX_FLAG, Value::Int(i64::from(source.flag_id)))];
                if source.owner_id != identity.char_id {
                    changes.push((IX_OWNER_ID, Value::Int(source.owner_id)));
                }
                dst_payloads.push((row.row(), item_change_dict(changes)));
            }
        }
        self.npc_wreck_emit(root, type_id, root_flag, src_payloads);
        self.npc_wreck_emit(dest, dest_type, dest_flag, dst_payloads);
        self.npc_wreck_refresh_is_empty(root, was_src_empty);
        self.npc_wreck_refresh_is_empty(dest, was_dst_empty);
        self.npc_wreck_ensure_ids();
        Ok(new_id)
    }
}
