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

        let rated_key = DataKey2::EngagementRated(engagement_id.clone());
        if env.storage().persistent().has(&rated_key) {
            panic!("AlreadyRated");
        }
        env.storage().persistent().set(&rated_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&rated_key, 100_000, 6_300_000);

        let rating_key = DataKey2::RecruiterRating(engagement.recruiter.clone());
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

        // ISSUE #596 — recency-weighted rating. Decay the running weighted
        // totals by the configured half-life (in ledgers) before folding in
        // the new rating, so older ratings contribute exponentially less.
        let weighted_key = DataKey3::RecruiterWeightedRating(engagement.recruiter.clone());
        let mut weighted: WeightedRatingSummary = env
            .storage()
            .persistent()
            .get(&weighted_key)
            .unwrap_or(WeightedRatingSummary {
                weighted_stars: 0,
                weighted_count: 0,
                last_ledger: env.ledger().sequence(),
            });
        Self::decay_weighted_rating(&env, &mut weighted);
        weighted.weighted_stars += (stars as u64) * WEIGHTED_RATING_SCALE;
        weighted.weighted_count += WEIGHTED_RATING_SCALE;
        weighted.last_ledger = env.ledger().sequence();
        env.storage().persistent().set(&weighted_key, &weighted);
        env.storage()
            .persistent()
            .extend_ttl(&weighted_key, 100_000, 6_300_000);

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
            .get(&DataKey2::RecruiterRating(recruiter))
    }

    // ----------------------------------------------------------
    // ISSUE #596 — RECENCY-WEIGHTED RECRUITER RATING
    // ----------------------------------------------------------

    /// Admin configures the half-life (in ledgers) used to exponentially decay
    /// a recruiter's weighted rating. A half-life of 0 disables decay (the
    /// weighted rating then tracks the lifetime average).
    pub fn set_weighted_rating_half_life(env: Env, admin: Address, half_life_ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage().instance().set(
            &DataKey::Config(ConfigKey::WeightedRatingHalfLife),
            &half_life_ledgers,
        );
        env.events().publish(
            (Symbol::new(&env, "weighted_hl_set"),),
            half_life_ledgers,
        );
    }

    /// Return the configured weighted-rating half-life in ledgers (0 if unset).
    pub fn get_weighted_rating_half_life(env: Env) -> u32 {
        Self::get_weighted_rating_half_life_internal(&env)
    }

    /// Return the recruiter's recency-weighted rating as stars × 100, or `None`
    /// if they have never been rated. The stored weighted totals are decayed to
    /// the current ledger before the average is computed.
    pub fn get_recruiter_recent_rating(env: Env, recruiter: Address) -> Option<u32> {
        let key = DataKey3::RecruiterWeightedRating(recruiter);
        let mut weighted: WeightedRatingSummary = env.storage().persistent().get(&key)?;
        if weighted.weighted_count == 0 {
            return None;
        }
        Self::decay_weighted_rating(&env, &mut weighted);
        let stars_x100 = weighted
            .weighted_stars
            .saturating_mul(100)
            / weighted.weighted_count;
        Some(stars_x100 as u32)
    }

    pub(crate) fn get_weighted_rating_half_life_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::WeightedRatingHalfLife))
            .unwrap_or(0)
    }

    /// Apply exponential decay to `weighted` for the ledgers elapsed since its
    /// `last_ledger`, using the configured half-life. Decay is applied in
    /// `WEIGHTED_RATING_SCALE` fixed-point units; a zero half-life is a no-op.
    pub(crate) fn decay_weighted_rating(env: &Env, weighted: &mut WeightedRatingSummary) {
        let half_life = Self::get_weighted_rating_half_life_internal(env);
        let now = env.ledger().sequence();
        if half_life == 0 || now <= weighted.last_ledger {
            weighted.last_ledger = now;
            return;
        }
        let elapsed = (now - weighted.last_ledger) as u64;
        let factor = Self::decay_factor(elapsed, half_life as u64);
        weighted.weighted_stars = weighted.weighted_stars.saturating_mul(factor) / WEIGHTED_RATING_SCALE;
        weighted.weighted_count = weighted.weighted_count.saturating_mul(factor) / WEIGHTED_RATING_SCALE;
        weighted.last_ledger = now;
    }

    /// Deterministic fixed-point decay factor for `elapsed` ledgers given a
    /// `half_life`, expressed in `WEIGHTED_RATING_SCALE` units. Computed as
    /// `SCALE * (1/2)^(elapsed/half_life)` using integer halving steps so the
    /// result is fully deterministic across platforms.
    pub(crate) fn decay_factor(elapsed: u64, half_life: u64) -> u64 {
        if half_life == 0 {
            return WEIGHTED_RATING_SCALE;
        }
        let mut factor = WEIGHTED_RATING_SCALE;
        let mut remaining = elapsed;
        while remaining >= half_life {
            factor /= 2;
            remaining -= half_life;
            if factor == 0 {
                return 0;
            }
        }
        // Linear interpolation for the sub-half-life remainder keeps the curve
        // smooth while staying integer-deterministic.
        if remaining > 0 {
            let step = WEIGHTED_RATING_SCALE / 2;
            let drop = step.saturating_mul(remaining) / half_life;
            factor = factor.saturating_sub(drop);
        }
        factor
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

    /// Admin selects whether the proof cooldown discount uses the recruiter's
    /// lifetime rating (`false`, the default) or their recency-weighted rating
    /// (`true`).
    pub fn set_cooldown_uses_recent_rating(env: Env, admin: Address, use_recent: bool) {
        Self::assert_admin(&env, &admin);
        env.storage().instance().set(
            &DataKey::Config(ConfigKey::CooldownUsesRecentRating),
            &use_recent,
        );
        env.events().publish(
            (Symbol::new(&env, "cooldown_recent_set"),),
            use_recent,
        );
    }

    /// Return whether the cooldown discount uses the recency-weighted rating.
    pub fn get_cooldown_uses_recent_rating(env: Env) -> bool {
        Self::cooldown_uses_recent_rating_internal(&env)
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

    pub(crate) fn cooldown_uses_recent_rating_internal(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::CooldownUsesRecentRating))
            .unwrap_or(false)
    }

    pub(crate) fn effective_proof_cooldown_internal(env: &Env, recruiter: &Address) -> u32 {
        let base = Self::get_proof_cooldown(env);
        let config = Self::get_proof_cooldown_discount_internal(env);
        if config.discount_per_star_ledgers == 0 {
            return base;
        }

        // ISSUE #596 — the admin flag selects lifetime vs recency-weighted
        // rating for the discount. Both are expressed as stars × 100 so the
        // fractional discount maths is shared.
        let stars_x100: u64 = if Self::cooldown_uses_recent_rating_internal(env) {
            match Self::get_recruiter_recent_rating(env.clone(), recruiter.clone()) {
                Some(v) => v as u64,
                None => return base,
            }
        } else {
            let summary: Option<RatingSummary> = env
                .storage()
                .persistent()
                .get(&DataKey2::RecruiterRating(recruiter.clone()));
            match summary {
                Some(s) if s.rating_count > 0 => {
                    s.total_stars.saturating_mul(100) / s.rating_count as u64
                }
                _ => return base,
            }
        };

        let discount = stars_x100.saturating_mul(config.discount_per_star_ledgers as u64) / 100;
        let discounted = (base as u64).saturating_sub(discount) as u32;
        discounted.max(config.min_cooldown_ledgers).min(base)
    }
}
