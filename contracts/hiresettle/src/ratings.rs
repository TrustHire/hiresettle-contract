use soroban_sdk::{contractimpl, Address, Env, String, Symbol};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #470 — RECRUITER RATINGS
    // ----------------------------------------------------------

    /// Company rates the recruiter of a completed engagement with 1–5 stars.
    /// Each engagement can be rated at most once; the rating is added to the
    /// recruiter's running `RatingSummary`.
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused.
    /// - `"InvalidRating"` — `stars` is outside 1–5.
    /// - `"unauthorized"` — caller is not the engagement's company or co-signer.
    /// - `"EngagementNotCompleted"` — the engagement is not `Completed`.
    /// - `"AlreadyRated"` — this engagement has already been rated.
    ///
    /// # Events
    /// Emits `("recruiter_rated", engagement_id)` with `(recruiter, stars)`.
    pub fn rate_recruiter(env: Env, company: Address, engagement_id: String, stars: u32) {
        Self::assert_not_paused(&env);
        company.require_auth();

        if !(MIN_RATING_STARS..=MAX_RATING_STARS).contains(&stars) {
            panic!("InvalidRating");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if engagement.status != EngagementStatus::Completed {
            panic!("EngagementNotCompleted");
        }

        let rated_key = DataKey::EngagementRated(engagement_id.clone());
        if env.storage().persistent().has(&rated_key) {
            panic!("AlreadyRated");
        }
        env.storage().persistent().set(&rated_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&rated_key, 100_000, 6_300_000);

        let rating_key = DataKey::RecruiterRating(engagement.recruiter.clone());
        let mut summary: RatingSummary =
            env.storage()
                .persistent()
                .get(&rating_key)
                .unwrap_or(RatingSummary {
                    total_stars: 0,
                    rating_count: 0,
                });
        summary.total_stars += stars as u64;
        summary.rating_count += 1;
        env.storage().persistent().set(&rating_key, &summary);
        env.storage()
            .persistent()
            .extend_ttl(&rating_key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "recruiter_rated"), engagement_id),
            (engagement.recruiter, stars),
        );
    }

    /// Return the recruiter's accumulated rating, or `None` if they have never
    /// been rated.
    pub fn get_recruiter_rating(env: Env, recruiter: Address) -> Option<RatingSummary> {
        env.storage()
            .persistent()
            .get(&DataKey::RecruiterRating(recruiter))
    }

    // ----------------------------------------------------------
    // ISSUE #470 — RATING-BASED PROOF COOLDOWN DISCOUNT
    // ----------------------------------------------------------

    /// Admin configures how much a recruiter's average rating shortens their
    /// proof resubmission cooldown, and the floor it can never drop below.
    /// Setting `discount_per_star_ledgers` to 0 disables the discount.
    ///
    /// Named `set_cooldown_rating_discount` rather than issue #470's
    /// `set_proof_cooldown_rating_discount` to fit Soroban's 32-character
    /// contract function name limit.
    pub fn set_cooldown_rating_discount(
        env: Env,
        admin: Address,
        discount_per_star_ledgers: u32,
        min_cooldown_ledgers: u32,
    ) {
        Self::assert_admin(&env, &admin);
        env.storage().instance().set(
            &DataKey::Config(ConfigKey::ProofCooldownDiscount),
            &ProofCooldownDiscount {
                discount_per_star_ledgers,
                min_cooldown_ledgers,
            },
        );
        env.events().publish(
            (Symbol::new(&env, "cooldown_discount_set"),),
            (discount_per_star_ledgers, min_cooldown_ledgers),
        );
    }

    /// Return the configured cooldown discount curve (all zeros if unset).
    pub fn get_cooldown_rating_discount(env: Env) -> ProofCooldownDiscount {
        Self::get_proof_cooldown_discount_internal(&env)
    }

    /// Return the proof resubmission cooldown that applies to `recruiter`:
    /// `max(min_cooldown_ledgers, base_cooldown - average_stars * discount_per_star_ledgers)`,
    /// using the recruiter's rating at read time.
    ///
    /// An unrated recruiter, or an unconfigured discount, gets the full base
    /// cooldown. The average is applied fractionally (`total_stars *
    /// discount / rating_count`, rounded down), and the result never exceeds
    /// the base cooldown, even if the floor is configured above it.
    pub fn get_effective_proof_cooldown(env: Env, recruiter: Address) -> u32 {
        Self::effective_proof_cooldown_internal(&env, &recruiter)
    }

    pub(crate) fn get_proof_cooldown_discount_internal(env: &Env) -> ProofCooldownDiscount {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ProofCooldownDiscount))
            .unwrap_or(ProofCooldownDiscount {
                discount_per_star_ledgers: 0,
                min_cooldown_ledgers: 0,
            })
    }

    pub(crate) fn effective_proof_cooldown_internal(env: &Env, recruiter: &Address) -> u32 {
        let base = Self::get_proof_cooldown(env);
        let config = Self::get_proof_cooldown_discount_internal(env);
        if config.discount_per_star_ledgers == 0 {
            return base;
        }

        let summary: Option<RatingSummary> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterRating(recruiter.clone()));
        let summary = match summary {
            Some(s) if s.rating_count > 0 => s,
            _ => return base,
        };

        let discount = summary
            .total_stars
            .saturating_mul(config.discount_per_star_ledgers as u64)
            / summary.rating_count as u64;
        let discounted = (base as u64).saturating_sub(discount) as u32;
        discounted.max(config.min_cooldown_ledgers).min(base)
    }
}
