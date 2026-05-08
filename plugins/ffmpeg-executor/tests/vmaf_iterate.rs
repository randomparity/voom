use std::cell::RefCell;
use std::path::Path;

use voom_ffmpeg_executor::vmaf::{SampleError, SampleExtractor, VmafError, VmafModel};
use voom_ffmpeg_executor::vmaf_iterate::{
    iterate_to_target_with, BitrateBounds, EncodeAttempt, IterationResult,
};

struct MockSampleExtractor;

impl SampleExtractor for MockSampleExtractor {
    fn extract(&self, _source: &Path, dest: &Path) -> Result<(), SampleError> {
        std::fs::write(dest, b"sample").map_err(SampleError::Io)
    }
}

struct MockProbe {
    attempts: RefCell<Vec<(u32, f64)>>,
}

impl MockProbe {
    fn new(attempts: Vec<(u32, f64)>) -> Self {
        Self {
            attempts: RefCell::new(attempts),
        }
    }
}

impl EncodeAttempt for MockProbe {
    fn encode_sample(
        &self,
        _source: &Path,
        _dest: &Path,
        crf: u32,
        _bounds: BitrateBounds,
    ) -> Result<Option<String>, VmafError> {
        let attempts = self.attempts.borrow();
        assert!(
            attempts
                .iter()
                .any(|(expected_crf, _)| *expected_crf == crf),
            "unexpected CRF {crf}"
        );
        Ok(Some(format!("{}k", 8_000_u32.saturating_sub(crf * 100))))
    }

    fn compute_vmaf(
        &self,
        _reference: &Path,
        _distorted: &Path,
        _model: VmafModel,
    ) -> Result<f64, VmafError> {
        let mut attempts = self.attempts.borrow_mut();
        assert!(!attempts.is_empty(), "no VMAF score left for attempt");
        Ok(attempts.remove(0).1)
    }
}

#[test]
fn iterate_to_target_converges_by_bisecting_crf() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();
    let probe = MockProbe::new(vec![(23, 88.0), (11, 96.0), (17, 92.2)]);

    let result = iterate_to_target_with(
        &source,
        92,
        BitrateBounds::default(),
        &MockSampleExtractor,
        5,
        &probe,
    )
    .unwrap();

    assert_eq!(
        result,
        IterationResult {
            final_crf: 17,
            final_bitrate: Some("6300k".to_string()),
            achieved_vmaf: 92.2,
            iterations: 3,
        }
    );
}

#[test]
fn iterate_to_target_stops_with_libvmaf_unavailable() {
    struct UnavailableProbe;

    impl EncodeAttempt for UnavailableProbe {
        fn encode_sample(
            &self,
            _source: &Path,
            _dest: &Path,
            _crf: u32,
            _bounds: BitrateBounds,
        ) -> Result<Option<String>, VmafError> {
            Ok(None)
        }

        fn compute_vmaf(
            &self,
            _reference: &Path,
            _distorted: &Path,
            _model: VmafModel,
        ) -> Result<f64, VmafError> {
            Err(VmafError::LibvmafUnavailable)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();

    let err = iterate_to_target_with(
        &source,
        92,
        BitrateBounds::default(),
        &MockSampleExtractor,
        5,
        &UnavailableProbe,
    )
    .unwrap_err();

    assert!(err.to_string().contains("ffmpeg does not support libvmaf"));
}

#[test]
fn bitrate_bounds_are_passed_to_every_encode_attempt() {
    struct BoundsProbe {
        seen: RefCell<Vec<BitrateBounds>>,
    }

    impl EncodeAttempt for BoundsProbe {
        fn encode_sample(
            &self,
            _source: &Path,
            _dest: &Path,
            _crf: u32,
            bounds: BitrateBounds,
        ) -> Result<Option<String>, VmafError> {
            self.seen.borrow_mut().push(bounds);
            Ok(Some("4M".to_string()))
        }

        fn compute_vmaf(
            &self,
            _reference: &Path,
            _distorted: &Path,
            _model: VmafModel,
        ) -> Result<f64, VmafError> {
            Ok(91.0)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();
    let bounds = BitrateBounds {
        min_bitrate: Some("2M".to_string()),
        max_bitrate: Some("8M".to_string()),
    };
    let probe = BoundsProbe {
        seen: RefCell::new(Vec::new()),
    };

    let _ = iterate_to_target_with(&source, 92, bounds.clone(), &MockSampleExtractor, 5, &probe)
        .unwrap();

    assert_eq!(probe.seen.into_inner(), vec![bounds]);
}
