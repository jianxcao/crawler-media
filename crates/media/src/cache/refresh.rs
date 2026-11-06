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
    pub(super) fn acquire() -> Result<Self, TmdbError> {
        let mut counts = budget().counts.lock();
        loop {
            crate::catalog_request::check_current()?;
            if counts.active < 8 {
                break;
            }
            if let Some(request) = crate::CatalogRequest::current() {
                let remaining = request
                    .deadline()
                    .saturating_duration_since(std::time::Instant::now());
                budget().ready.wait_for(
                    &mut counts,
                    remaining.min(std::time::Duration::from_millis(20)),
                );
            } else {
                budget().ready.wait(&mut counts);
            }
        }
        counts.active += 1;
        Ok(Self)
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
    admission: Option<Admission>,
}
impl Leader<'_> {
    fn release(&mut self) {
        let mut flights = self.flights.0.lock();
        if flights
            .get(self.key)
            .is_some_and(|flight| Arc::ptr_eq(flight, &self.flight))
        {
            flights.remove(self.key);
        }
        drop(flights);
        drop(self.admission.take());
    }

    fn publish(&self, outcome: Outcome) {
        *self.flight.result.lock() = Some(outcome);
        self.flight.ready.notify_all();
    }

    fn complete(mut self, outcome: Outcome) -> Outcome {
        // Free this leader's admission before a live follower can re-elect.
        self.release();
        self.publish(outcome.clone());
        outcome
    }
}
impl Drop for Leader<'_> {
    fn drop(&mut self) {
        self.release();
        if self.flight.result.lock().is_none() {
            self.publish(Err(TmdbError::Http("catalog refresh interrupted".into())));
        }
    }
}
impl Flights {
    pub(super) fn run(
        &self,
        key: &str,
        work: impl FnOnce() -> Outcome,
    ) -> Result<Outcome, CapacityExhausted> {
        loop {
            if let Err(error) = crate::catalog_request::check_current() {
                return Ok(Err(error));
            }
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
                    admission: Some(admission),
                };
                let outcome = work();
                return Ok(leader.complete(outcome));
            } else {
                let mut result = flight.result.lock();
                while result.is_none() {
                    if let Some(request) = crate::CatalogRequest::current() {
                        if let Err(error) = request.check() {
                            return Ok(Err(error));
                        }
                        let remaining = request
                            .deadline()
                            .saturating_duration_since(std::time::Instant::now());
                        flight.ready.wait_for(
                            &mut result,
                            remaining.min(std::time::Duration::from_millis(20)),
                        );
                    } else {
                        flight.ready.wait(&mut result);
                    }
                }
                if let Err(error) = crate::catalog_request::check_current() {
                    return Ok(Err(error));
                }
                // The loop establishes a completed result without a panicking unwrap.
                let outcome = result.clone().unwrap_or_else(|| {
                    Err(TmdbError::Http(
                        "catalog refresh completed without a result".into(),
                    ))
                });
                drop(result);
                if matches!(outcome, Err(TmdbError::Deadline | TmdbError::Cancelled)) {
                    // This caller still has its own live budget. The old leader
                    // has ended; it must not cancel an independent observer.
                    continue;
                }
                return Ok(outcome);
            }
        }
    }
}
