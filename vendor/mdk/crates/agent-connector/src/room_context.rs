//! Read authenticated account identities and addressing without parsing text.
use crate::{AgentConnector, error::ConnectorError, validation::normalize_hex};
use agent_control::{AgentControlResponse, AgentControlRoomMessage};

fn addressed_accounts(tags: &[Vec<String>]) -> Vec<String> {
    tags.iter()
        .filter(|tag| tag.first().is_some_and(|name| name == "p"))
        .filter_map(|tag| tag.get(1))
        .filter_map(|id| normalize_hex(id).ok())
        .filter(|id| id.len() == 64)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl AgentConnector {
    pub(crate) async fn room_context_response(
        &self,
        account: &str,
        group: &str,
        message: Option<&str>,
    ) -> Result<AgentControlResponse, ConnectorError> {
        let account = self.local_account_for_account_id(account)?;
        let group = normalize_hex(group)?;
        let group_id = cgka_traits::GroupId::new(hex::decode(&group)?);
        // Runtime account projections group authenticated MLS leaves by account.
        // A total member/leaf count or a display profile cannot substitute here.
        let rosters = self
            .runtime
            .group_member_ids_page(&account.label, std::slice::from_ref(&group_id))
            .await?;
        let members = rosters
            .into_iter()
            .find(|row| row.group_id_hex == group)
            .ok_or(ConnectorError::MemberResolutionFailed)?
            .member_ids_hex;
        let message = match message {
            None => None,
            Some(id) => self
                .runtime
                .timeline_message(&account.label, &group, &normalize_hex(id)?)?
                .filter(|row| {
                    !row.deleted
                        && row.invalidation_status.is_none()
                        && cgka_traits::is_chat_message(row.kind, &row.tags)
                })
                .map(|row| AgentControlRoomMessage {
                    message_id_hex: row.message_id_hex,
                    sender_account_id_hex: row.sender,
                    addressed_accounts: addressed_accounts(&row.tags),
                    unsupported_media: row.plaintext.len() > 30000
                        || row
                            .tags
                            .iter()
                            .any(|tag| tag.first().is_some_and(|name| name == "imeta")),
                    text: if row.plaintext.len() <= 30000 {
                        row.plaintext
                    } else {
                        String::new()
                    },
                    recorded_at: row.timeline_at,
                }),
        };
        Ok(AgentControlResponse::RoomContext {
            account_id_hex: account.account_id_hex,
            group_id_hex: group,
            members,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_addresses_are_only_canonical_inner_p_tags() {
        assert!(addressed_accounts(&[]).is_empty());
        assert!(
            addressed_accounts(&[vec!["text".into(), format!("@{}", "aa".repeat(32))]]).is_empty()
        );
        assert_eq!(
            addressed_accounts(&[
                vec!["p".into(), "AA".repeat(32)],
                vec!["p".into(), "aa".repeat(32)]
            ]),
            vec!["aa".repeat(32)]
        );
    }
}
