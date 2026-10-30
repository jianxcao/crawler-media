//! Caller-thread refresh admission and result sharing; locks never cover source IO.
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::{Condvar, Mutex};

use crate::TmdbError;

type Outcome = Result<Arc<str>, TmdbError>;
#[derive(Default)]
pub(super) struct Flights(Mutex<HashMap<String, Arc<Flight>>>);
#[derive(Default)]
struct Flight {
    result: Mutex<Option<Outcome>>,
    ready: Condvar,
}
#[derive(Default)]
struct Counts {
    leaders: usize,
    active: usize,
}
#[derive(Default)]
struct Budget {
    counts: Mutex<Counts>,
    ready: Condvar,
}
fn budget() -> &'static Budget {
    static BUDGET: OnceLock<Budget> = OnceLock::new();
    BUDGET.get_or_init(Budget::default)
}
pub(super) struct CapacityExhausted;
struct Admission;
impl Admission {
    fn acquire() -> Result<Self, CapacityExhausted> {
        let mut counts = budget().counts.lock();
        if counts.leaders >= 64 {
            return Err(CapacityExhausted);
        }
        counts.leaders += 1;
        Ok(Self)
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        budget().counts.lock().leaders -= 1;
        budget().ready.notify_all();
    }
}
pub(super) struct OriginPermit;
impl OriginPermit {
    pub(super) fn acquire() -> Self {
        let mut counts = budget().counts.lock();
        while counts.active >= 8 {
            budget().ready.wait(&mut counts);
        }
        counts.active += 1;
        Self
    }
}
impl Drop for OriginPermit {
    fn drop(&mut self) {
        budget().counts.lock().active -= 1;
        budget().ready.notify_one();
    }
}
struct Leader<'a> {
    flights: &'a Flights,
    key: &'a str,
    flight: Arc<Flight>,
    _admission: Admission,
}
impl Leader<'_> {
    fn publish(&self, outcome: Outcome) {
        *self.flight.result.lock() = Some(outcome);
        self.flight.ready.notify_all();
    }
}
impl Drop for Leader<'_> {
    fn drop(&mut self) {
        if self.flight.result.lock().is_none() {
            self.publish(Err(TmdbError::Http("catalog refresh interrupted".into())));
        }
        self.flights.0.lock().remove(self.key);
    }
}
impl Flights {
    pub(super) fn run(
        &self,
        key: &str,
        work: impl FnOnce() -> Outcome,
    ) -> Result<Outcome, CapacityExhausted> {
        let (flight, admission) = {
            let mut flights = self.0.lock();
            if let Some(flight) = flights.get(key) {
                (flight.clone(), None)
            } else {
                let admission = Admission::acquire()?;
                let flight = Arc::new(Flight::default());
                flights.insert(key.to_string(), flight.clone());
                (flight, Some(admission))
            }
        };
        if let Some(admission) = admission {
            let leader = Leader {
                flights: self,
                key,
                flight,
                _admission: admission,
            };
            let outcome = work();
            leader.publish(outcome.clone());
            Ok(outcome)
        } else {
            let mut result = flight.result.lock();
            while result.is_none() {
                flight.ready.wait(&mut result);
            }
            // The loop establishes a completed result without a panicking unwrap.
            Ok(result.clone().unwrap_or_else(|| {
                Err(TmdbError::Http(
                    "catalog refresh completed without a result".into(),
                ))
            }))
        }
    }
}
