//! Output routing: the foreign [`OutputRouter`] trait that the Android shell implements, and the
//! adapter that presents it to the engine as a [`RoutePlanner`].
//!
//! The adapter guards every foreign call. A Kotlin exception surfaces as a panic in UniFFI's
//! callback glue; the adapter catches it, logs it and falls back to the default route, so a buggy
//! router cannot stop playback.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use sonorus_engine::aaudio::{DefaultPlanner, RoutePlan, RoutePlanner, RouteRequest, SignalPath};

use crate::types::{RoutePlanInfo, RouteRequestInfo, SignalPathInfo};

/// Chooses the output route (USB DAC, bit-perfect mixer format) for each stream the engine opens.
/// Implemented in Kotlin.
///
/// Called off the main thread, possibly from two threads at once; may block briefly.
#[uniffi::export(with_foreign)]
pub trait OutputRouter: Send + Sync {
    /// Plans the open for `request`.
    fn plan(&self, request: RouteRequestInfo) -> RoutePlanInfo;
    /// Reports the outcome of an open attempt.
    fn opened(&self, path: SignalPathInfo);
}

/// Engine planner that asks an [`OutputRouter`]. A panicking router yields the default route.
pub(crate) struct RouterPlanner(Arc<dyn OutputRouter>);

impl RouterPlanner {
    /// Wraps `router` for the engine.
    pub(crate) fn new(router: Arc<dyn OutputRouter>) -> Self {
        Self(router)
    }
}

impl RoutePlanner for RouterPlanner {
    fn plan(&self, request: &RouteRequest) -> RoutePlan {
        let info = RouteRequestInfo::from(*request);
        let router = &self.0;
        match catch_unwind(AssertUnwindSafe(|| router.plan(info))) {
            Ok(plan) => RoutePlan::from(plan),
            Err(_) => {
                log::warn!("output router failed; using default route");
                DefaultPlanner.plan(request)
            }
        }
    }

    fn opened(&self, path: &SignalPath) {
        let info = SignalPathInfo::from(path.clone());
        let router = &self.0;
        if catch_unwind(AssertUnwindSafe(|| router.opened(info))).is_err() {
            log::warn!("output router failed on opened; ignored");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use sonorus_engine::aaudio::{AAudioConfig, PathMode, Performance, Sharing};
    use sonorus_engine::output::SampleKind;

    use super::*;
    use crate::types::{OutputPathMode, OutputSampleFormat};

    /// Returns a fixed plan and records what it is told about opens.
    struct FixedRouter {
        plan: RoutePlanInfo,
        opened: Mutex<Vec<SignalPathInfo>>,
    }

    impl OutputRouter for FixedRouter {
        fn plan(&self, _request: RouteRequestInfo) -> RoutePlanInfo {
            self.plan
        }

        fn opened(&self, path: SignalPathInfo) {
            self.opened
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(path);
        }
    }

    /// Panics on every call, like a Kotlin exception in the callback glue.
    struct PanickingRouter;

    impl OutputRouter for PanickingRouter {
        fn plan(&self, _request: RouteRequestInfo) -> RoutePlanInfo {
            panic!("router bug");
        }

        fn opened(&self, _path: SignalPathInfo) {
            panic!("router bug");
        }
    }

    fn request() -> RouteRequest {
        RouteRequest {
            sample_rate: 96_000,
            channels: 2,
            source_bits: Some(24),
            requested: SampleKind::I24Packed,
            config: AAudioConfig {
                sharing: Sharing::Exclusive,
                performance: Performance::PowerSaving,
                device_id: None,
            },
        }
    }

    fn usb_plan() -> RoutePlanInfo {
        RoutePlanInfo {
            device_id: Some(12),
            sample_format: OutputSampleFormat::Int32,
            exclusive: true,
            bit_perfect: true,
            hold: false,
        }
    }

    fn path() -> SignalPath {
        SignalPath {
            mode: PathMode::BitPerfect,
            plan: RoutePlan {
                device_id: Some(12),
                sample: SampleKind::I32,
                sharing: Sharing::Exclusive,
                performance: Performance::LowLatency,
                bit_perfect: true,
                hold: false,
            },
            stream: None,
            error: None,
            source_rate: 96_000,
            source_bits: Some(24),
        }
    }

    #[test]
    fn planner_uses_router_plan() {
        let router = Arc::new(FixedRouter {
            plan: usb_plan(),
            opened: Mutex::new(Vec::new()),
        });
        let planner = RouterPlanner::new(router);
        let plan = planner.plan(&request());
        assert_eq!(
            plan,
            RoutePlan {
                device_id: Some(12),
                sample: SampleKind::I32,
                sharing: Sharing::Exclusive,
                performance: Performance::LowLatency,
                bit_perfect: true,
                hold: false,
            }
        );
    }

    #[test]
    fn planner_forwards_opened_to_router() {
        let router = Arc::new(FixedRouter {
            plan: usb_plan(),
            opened: Mutex::new(Vec::new()),
        });
        let planner = RouterPlanner::new(router.clone());
        planner.opened(&path());
        let seen = router
            .opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].mode, OutputPathMode::BitPerfect);
        assert_eq!(seen[0].plan, usb_plan());
        assert_eq!(seen[0].source_bits, Some(24));
    }

    #[test]
    fn panicking_router_falls_back_to_default_plan() {
        let planner = RouterPlanner::new(Arc::new(PanickingRouter));
        assert_eq!(
            planner.plan(&request()),
            DefaultPlanner.plan(&request()),
            "a panicking router must yield the default route"
        );
    }

    #[test]
    fn panicking_router_does_not_unwind_into_engine_on_opened() {
        let planner = RouterPlanner::new(Arc::new(PanickingRouter));
        planner.opened(&path());
    }
}
