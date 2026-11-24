//! The dispute lifecycle of a deposit, as one exhaustive state machine.
//!
//! ```text
//!              Dispute               Chargeback
//! Undisputed ──────────▶ Disputed ──────────────▶ ChargedBack (final)
//!      ▲                    │
//!      └────── Resolve ─────┘
//! ```
//!
//! A resolve returns the deposit to `Undisputed`, so it can be disputed again:
//! the spec treats only a chargeback as final.

use crate::amount::Amount;
use crate::error::TxError;
use crate::transaction::ClientId;

/// Where a deposit is in its dispute lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeState {
    /// Not under dispute; the funds are available to the client.
    Undisputed,
    /// Under dispute; the funds are held.
    Disputed,
    /// Reversed; the funds are gone and nothing can happen to this deposit any more.
    ChargedBack,
}

/// Something that moves a deposit through its dispute lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeEvent {
    Dispute,
    Resolve,
    Chargeback,
}

impl DisputeState {
    /// The state after `event`, or why `event` is not allowed in this state.
    ///
    /// Every (state, event) pair is listed, with no catch-all arm, so adding a
    /// state or an event is a compile error here until its transitions are decided.
    pub fn next(self, event: DisputeEvent) -> Result<Self, TxError> {
        use DisputeEvent as Event;
        use DisputeState as State;

        match (self, event) {
            (State::Undisputed, Event::Dispute) => Ok(State::Disputed),
            (State::Undisputed, Event::Resolve | Event::Chargeback) => Err(TxError::NotDisputed),
            (State::Disputed, Event::Dispute) => Err(TxError::AlreadyDisputed),
            (State::Disputed, Event::Resolve) => Ok(State::Undisputed),
            (State::Disputed, Event::Chargeback) => Ok(State::ChargedBack),
            (State::ChargedBack, Event::Dispute | Event::Resolve | Event::Chargeback) => {
                Err(TxError::AlreadyChargedBack)
            }
        }
    }
}

/// What the engine remembers about an accepted deposit, so that later
/// disputes, resolves and chargebacks can refer to it by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepositRecord {
    /// Owner of the deposit; a dispute naming another client is ignored.
    pub client: ClientId,
    /// The amount to hold, release or charge back.
    pub amount: Amount,
    pub state: DisputeState,
}

impl DepositRecord {
    /// A freshly accepted, undisputed deposit.
    pub fn new(client: ClientId, amount: Amount) -> Self {
        Self {
            client,
            amount,
            state: DisputeState::Undisputed,
        }
    }
}

// One record is kept per accepted deposit, so its size drives the engine's
// memory use. Changing it should be a deliberate decision.
const _: () = assert!(std::mem::size_of::<DepositRecord>() == 20);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_transition() {
        use DisputeEvent::{Chargeback, Dispute, Resolve};
        use DisputeState::{ChargedBack, Disputed, Undisputed};

        let cases = [
            (Undisputed, Dispute, Ok(Disputed)),
            (Undisputed, Resolve, Err(TxError::NotDisputed)),
            (Undisputed, Chargeback, Err(TxError::NotDisputed)),
            (Disputed, Dispute, Err(TxError::AlreadyDisputed)),
            (Disputed, Resolve, Ok(Undisputed)),
            (Disputed, Chargeback, Ok(ChargedBack)),
            (ChargedBack, Dispute, Err(TxError::AlreadyChargedBack)),
            (ChargedBack, Resolve, Err(TxError::AlreadyChargedBack)),
            (ChargedBack, Chargeback, Err(TxError::AlreadyChargedBack)),
        ];
        for (state, event, expected) in cases {
            assert_eq!(state.next(event), expected, "{state:?} + {event:?}");
        }
    }

    #[test]
    fn new_deposit_record_is_undisputed() {
        let record = DepositRecord::new(ClientId(1), Amount::MAX);
        assert_eq!(record.state, DisputeState::Undisputed);
    }
}
