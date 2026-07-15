#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! Within-principal user identity store for Unicity AOS.
//!
//! Implements the `astrid:users@1.0.0` interface
//! (`astrid-runtime/wit/interfaces/users.wit`) over IPC RPC.
//!
//! Two-layer model:
//!
//! * **Identity layer** — `FrontendLink` maps a platform's stable
//!   opaque ID (Discord snowflake, Telegram user-id, Nostr pubkey)
//!   to a canonical `AstridUserId`. Global per
//!   `(platform, platform_instance, platform_user_id)` triple.
//! * **Presentation layer** — `ContextIdentity` overlays a
//!   per-context display name on a link (Discord guild nickname,
//!   Matrix room display name, Slack channel profile).
//!
//! `resolve` is context-aware: returns the layered display name in
//! one round-trip (context > link > canonical).

use astrid_sdk::prelude::*;
use uuid::Uuid;

mod requests;
mod responses;
mod store;
mod time;
mod types;

#[cfg(test)]
mod store_tests;

#[cfg(test)]
mod platform_scenarios;

pub use requests::{
    ContextClearRequest, ContextGetRequest, ContextListForUserRequest, ContextListInContextRequest,
    ContextSetRequest, CreateRequest, DeleteRequest, GetRequest, LinkRequest, LinksRequest,
    ListRequest, ResolveRequest, SetDisplayNameRequest, SetPublicKeyRequest, UnlinkRequest,
};
pub use responses::{context_to_json, link_to_json, user_to_json, user_value};
pub use store::{Backend, Page, SdkBackend, Store};
pub use types::{
    AstridUser, ContextIdentity, FrontendLink, ResolvedDisplayName, Source, StoreError,
    normalize_platform,
};

#[derive(Default)]
pub struct UsersCapsule;

impl UsersCapsule {
    fn store(&self) -> Store<SdkBackend> {
        Store::new(SdkBackend)
    }

    fn publish_error(topic: &str, correlation_id: &str, err: StoreError) {
        let _ = ipc::publish_json(
            topic,
            &serde_json::json!({
                "correlation-id": correlation_id,
                "error": err.to_string(),
            }),
        );
    }

    fn publish(topic: &str, payload: serde_json::Value) {
        let _ = ipc::publish_json(topic, &payload);
    }

    fn parse_uuid(s: &str, topic: &str, cid: &str) -> Option<Uuid> {
        match Uuid::parse_str(s) {
            Ok(u) => Some(u),
            Err(e) => {
                Self::publish_error(
                    topic,
                    cid,
                    StoreError::InvalidInput(format!("astrid_user_id is not a valid UUID: {e}")),
                );
                None
            }
        }
    }

    fn parse_pubkey(bytes: Option<&[u8]>, topic: &str, cid: &str) -> Option<Option<[u8; 32]>> {
        match bytes {
            None => Some(None),
            Some(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(b);
                Some(Some(arr))
            }
            Some(b) => {
                Self::publish_error(
                    topic,
                    cid,
                    StoreError::InvalidInput(format!(
                        "public_key must be 32 bytes (got {})",
                        b.len()
                    )),
                );
                None
            }
        }
    }
}

#[capsule]
impl UsersCapsule {
    #[astrid::interceptor("handle_resolve")]
    pub fn handle_resolve(&self, req: ResolveRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.resolve.response";
        let cid = &req.source.correlation_id;
        match self.store().resolve(
            &req.platform,
            req.platform_instance.as_deref(),
            &req.platform_user_id,
            req.context_id.as_deref(),
        ) {
            Ok((user, resolved)) => {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "correlation-id".into(),
                    serde_json::Value::String(cid.clone()),
                );
                obj.insert(
                    "user".into(),
                    user_to_json(user.as_ref()).unwrap_or(serde_json::Value::Null),
                );
                if let Some(r) = resolved {
                    obj.insert("display-name".into(), serde_json::Value::String(r.name));
                    obj.insert(
                        "display-name-source".into(),
                        serde_json::Value::String(r.source.to_string()),
                    );
                }
                Self::publish(TOPIC, serde_json::Value::Object(obj));
            }
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_link")]
    pub fn handle_link(&self, req: LinkRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.link.response";
        let cid = &req.source.correlation_id;
        let Some(astrid_id) = Self::parse_uuid(&req.astrid_user_id, TOPIC, cid) else {
            return Ok(());
        };
        match self.store().link(
            &req.platform,
            req.platform_instance.as_deref(),
            &req.platform_user_id,
            astrid_id,
            &req.method,
            req.display_name.as_deref(),
        ) {
            Ok(link) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "link": link_to_json(&link),
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_unlink")]
    pub fn handle_unlink(&self, req: UnlinkRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.unlink.response";
        let cid = &req.source.correlation_id;
        match self.store().unlink(
            &req.platform,
            req.platform_instance.as_deref(),
            &req.platform_user_id,
        ) {
            Ok(removed) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "removed": removed,
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_create")]
    pub fn handle_create(&self, req: CreateRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.create.response";
        let cid = &req.source.correlation_id;
        match self.store().create_user(req.display_name.as_deref()) {
            Ok(user) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "user": user_value(&user),
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_set_display_name")]
    pub fn handle_set_display_name(&self, req: SetDisplayNameRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.set_display_name.response";
        let cid = &req.source.correlation_id;
        let Some(astrid_id) = Self::parse_uuid(&req.astrid_user_id, TOPIC, cid) else {
            return Ok(());
        };
        match self
            .store()
            .set_display_name(astrid_id, req.display_name.as_deref())
        {
            Ok(user) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "user": user_value(&user),
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_set_public_key")]
    pub fn handle_set_public_key(&self, req: SetPublicKeyRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.set_public_key.response";
        let cid = &req.source.correlation_id;
        let Some(astrid_id) = Self::parse_uuid(&req.astrid_user_id, TOPIC, cid) else {
            return Ok(());
        };
        let Some(pk) = Self::parse_pubkey(req.public_key.as_deref(), TOPIC, cid) else {
            return Ok(());
        };
        match self.store().set_public_key(astrid_id, pk) {
            Ok(user) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "user": user_value(&user),
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_links")]
    pub fn handle_links(&self, req: LinksRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.links.response";
        let cid = &req.source.correlation_id;
        let Some(astrid_id) = Self::parse_uuid(&req.astrid_user_id, TOPIC, cid) else {
            return Ok(());
        };
        match self.store().list_links(astrid_id) {
            Ok(links) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "links": links.iter().map(link_to_json).collect::<Vec<_>>(),
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_get")]
    pub fn handle_get(&self, req: GetRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.get.response";
        let cid = &req.source.correlation_id;
        let Some(astrid_id) = Self::parse_uuid(&req.astrid_user_id, TOPIC, cid) else {
            return Ok(());
        };
        match self.store().get_user(astrid_id) {
            Ok(user) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "user": user_to_json(user.as_ref()).unwrap_or(serde_json::Value::Null),
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_delete")]
    pub fn handle_delete(&self, req: DeleteRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.delete.response";
        let cid = &req.source.correlation_id;
        let Some(astrid_id) = Self::parse_uuid(&req.astrid_user_id, TOPIC, cid) else {
            return Ok(());
        };
        match self.store().delete_user(astrid_id) {
            Ok(deleted) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "deleted": deleted,
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_list")]
    pub fn handle_list(&self, req: ListRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.list.response";
        let cid = &req.source.correlation_id;
        let limit = req.limit.map(|l| l as usize);
        match self
            .store()
            .list_users_paginated(req.cursor.as_deref(), limit)
        {
            Ok(page) => {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "correlation-id".into(),
                    serde_json::Value::String(cid.clone()),
                );
                obj.insert(
                    "users".into(),
                    serde_json::Value::Array(page.items.iter().map(user_value).collect()),
                );
                if let Some(c) = page.next_cursor {
                    obj.insert("next-cursor".into(), serde_json::Value::String(c));
                }
                Self::publish(TOPIC, serde_json::Value::Object(obj));
            }
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_context_set")]
    pub fn handle_context_set(&self, req: ContextSetRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.context.set.response";
        let cid = &req.source.correlation_id;
        match self.store().set_context(
            &req.platform,
            req.platform_instance.as_deref(),
            &req.platform_user_id,
            &req.context_id,
            &req.display_name,
        ) {
            Ok(overlay) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "context-identity": context_to_json(&overlay),
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_context_clear")]
    pub fn handle_context_clear(&self, req: ContextClearRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.context.clear.response";
        let cid = &req.source.correlation_id;
        match self.store().clear_context(
            &req.platform,
            req.platform_instance.as_deref(),
            &req.platform_user_id,
            &req.context_id,
        ) {
            Ok(removed) => Self::publish(
                TOPIC,
                serde_json::json!({
                    "correlation-id": cid,
                    "removed": removed,
                }),
            ),
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_context_get")]
    pub fn handle_context_get(&self, req: ContextGetRequest) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.context.get.response";
        let cid = &req.source.correlation_id;
        match self.store().get_context(
            &req.platform,
            req.platform_instance.as_deref(),
            &req.platform_user_id,
            &req.context_id,
        ) {
            Ok((overlay, astrid_id)) => {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "correlation-id".into(),
                    serde_json::Value::String(cid.clone()),
                );
                if let Some(o) = overlay {
                    obj.insert("context-identity".into(), context_to_json(&o));
                }
                if let Some(id) = astrid_id {
                    obj.insert(
                        "astrid-user-id".into(),
                        serde_json::Value::String(id.to_string()),
                    );
                }
                Self::publish(TOPIC, serde_json::Value::Object(obj));
            }
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_context_list_for_user")]
    pub fn handle_context_list_for_user(
        &self,
        req: ContextListForUserRequest,
    ) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.context.list_for_user.response";
        let cid = &req.source.correlation_id;
        let Some(astrid_id) = Self::parse_uuid(&req.astrid_user_id, TOPIC, cid) else {
            return Ok(());
        };
        let limit = req.limit.map(|l| l as usize);
        match self
            .store()
            .list_context_for_user(astrid_id, req.cursor.as_deref(), limit)
        {
            Ok(page) => {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "correlation-id".into(),
                    serde_json::Value::String(cid.clone()),
                );
                obj.insert(
                    "contexts".into(),
                    serde_json::Value::Array(page.items.iter().map(context_to_json).collect()),
                );
                if let Some(c) = page.next_cursor {
                    obj.insert("next-cursor".into(), serde_json::Value::String(c));
                }
                Self::publish(TOPIC, serde_json::Value::Object(obj));
            }
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }

    #[astrid::interceptor("handle_context_list_in_context")]
    pub fn handle_context_list_in_context(
        &self,
        req: ContextListInContextRequest,
    ) -> Result<(), SysError> {
        const TOPIC: &str = "users.v1.context.list_in_context.response";
        let cid = &req.source.correlation_id;
        let limit = req.limit.map(|l| l as usize);
        match self.store().list_context_in_context(
            &req.platform,
            req.platform_instance.as_deref(),
            &req.context_id,
            req.cursor.as_deref(),
            limit,
        ) {
            Ok(page) => {
                let members: Vec<serde_json::Value> = page
                    .items
                    .iter()
                    .map(|(ci, uid)| {
                        let mut m = serde_json::Map::new();
                        m.insert("context-identity".into(), context_to_json(ci));
                        if let Some(u) = uid {
                            m.insert(
                                "astrid-user-id".into(),
                                serde_json::Value::String(u.to_string()),
                            );
                        }
                        serde_json::Value::Object(m)
                    })
                    .collect();
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "correlation-id".into(),
                    serde_json::Value::String(cid.clone()),
                );
                obj.insert("members".into(), serde_json::Value::Array(members));
                if let Some(c) = page.next_cursor {
                    obj.insert("next-cursor".into(), serde_json::Value::String(c));
                }
                Self::publish(TOPIC, serde_json::Value::Object(obj));
            }
            Err(e) => Self::publish_error(TOPIC, cid, e),
        }
        Ok(())
    }
}
