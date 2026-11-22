//! Retained-search retainer milestones (issue #674).

use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    /// Claim one unlocked retainer instalment. Anyone may call on behalf of the
    /// engagement's recruiter; the funds always go to the recruiter side.
    ///
    /// # Panics
    /// - `"RetainerNotUnlocked"` before the next interval elapses.
    /// - `"RetainerCancelled"` once the company has cancelled future instalments.
    /// - `"RetainerAlreadyClaimed"` when the current instalment is still pending
    ///   proof-of-claim (the milestone stays `Pending` until the next interval).
    pub fn claim_retainer_instalment(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> i128 {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        let interval = match &milestone.kind {
            MilestoneKind::Retainer(interval_ledgers) => *interval_ledgers,
            _ => panic!("not a retainer milestone"),
        };
        if milestone.status != MilestoneStatus::Pending {
            panic!("RetainerAlreadyClaimed");
        }
        if env.storage().persistent().has(&DataKey3::RetainerCancelled(
            engagement_id.clone(),
            milestone_index,
        )) {
            panic!("RetainerCancelled");
        }
        // A paused engagement does not accrue unlocks: the clock restarts from
        // the unpause ledger, so no instalment unlocks while paused.
        let unlock_at = if let Some(paused_at) = env
            .storage()
            .persistent()
            .get::<_, u32>(&DataKey3::RetainerPausedAt(engagement_id.clone(), milestone_index))
        {
            paused_at.saturating_add(interval)
        } else {
            milestone.valid_after_ledger
        };
        let now = env.ledger().sequence();
        if now < unlock_at {
            panic!("RetainerNotUnlocked");
        }

        let gross = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        let fee_bps = Self::payout_fee_bps(&env, &engagement_id, &engagement);
        let fee = Self::platform_fee_amount(&env, &engagement_id, gross, fee_bps);
        let net = gross - fee;
        engagement.released_amount += gross;
        Self::add_token_locked(&env, &engagement.token, -gross);
        let token_client = token::Client::new(&env, &engagement.token);
        if fee > 0 {
            Self::collect_platform_fee(
                &env,
                &engagement,
                &engagement_id,
                milestone_index,
                fee,
            );
        }
        Self::distribute_recruiter_payout(&env, &engagement, net, &token_client, true);

        // Schedule the next instalment. The milestone never reaches a terminal
        // status on its own; the company may cancel future instalments.
        milestone.valid_after_ledger = now.saturating_add(interval);
        engagement
            .milestones
            .set(milestone_index, milestone.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        env.events().publish(
            (
                Symbol::new(&env, "retainer_instalment_claimed"),
                engagement_id,
            ),
            (milestone_index, gross, net),
        );
        net
    }

    /// Company cancels all future instalments of a retainer milestone. With
    /// `notice_ledgers > 0` the milestone only becomes terminal after the notice
    /// period; with `0` it is terminal immediately.
    pub fn cancel_retainer_future(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
        notice_ledgers: u32,
    ) {
        company.require_auth();
        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.company != company {
            panic!("unauthorized");
        }
        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if !matches!(&milestone.kind, MilestoneKind::Retainer(_)) {
            panic!("not a retainer milestone");
        }
        let cancel_at = env.ledger().sequence().saturating_add(notice_ledgers);
        milestone.valid_after_ledger = cancel_at;
        if notice_ledgers == 0 {
            milestone.status = MilestoneStatus::Confirmed;
        }
        engagement.milestones.set(milestone_index, milestone);
        env.storage()
            .persistent()
            .set(&DataKey3::RetainerCancelled(engagement_id.clone(), milestone_index), &cancel_at);
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        env.events().publish(
            (Symbol::new(&env, "retainer_cancelled"), engagement_id),
            (milestone_index, cancel_at),
        );
    }

    /// Mark retainer clocks as paused so no instalment unlocks during a pause.
    pub(crate) fn pause_retainer_clocks(env: &Env, engagement: &Engagement) {
        let now = env.ledger().sequence();
        for i in 0..engagement.milestones.len() {
            let m = engagement.milestones.get(i).unwrap();
            if matches!(&m.kind, MilestoneKind::Retainer(_))
                && m.status == MilestoneStatus::Pending
            {
                env.storage().persistent().set(
                    &DataKey3::RetainerPausedAt(engagement.id.clone(), i),
                    &now,
                );
            }
        }
    }

    /// Unpausing shifts each pending instalment's unlock ledger forward by the
    /// length of the pause.
    pub(crate) fn unpause_retainer_clocks(env: &Env, mut engagement: Engagement) {
        let now = env.ledger().sequence();
        for i in 0..engagement.milestones.len() {
            let mut m = engagement.milestones.get(i).unwrap();
            if let Some(paused_at) = env
                .storage()
                .persistent()
                .get::<_, u32>(&DataKey3::RetainerPausedAt(engagement.id.clone(), i))
            {
                let paused_for = now.saturating_sub(paused_at);
                env.storage()
                    .persistent()
                    .remove(&DataKey3::RetainerPausedAt(engagement.id.clone(), i));
                if matches!(&m.kind, MilestoneKind::Retainer(_))
                    && m.status == MilestoneStatus::Pending
                {
                    m.valid_after_ledger = m.valid_after_ledger.saturating_add(paused_for);
                    engagement.milestones.set(i, m);
                }
            }
        }
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement.id.clone()), &engagement);
    }
}
