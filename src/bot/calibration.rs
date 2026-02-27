/// Probability calibration utilities (Platt scaling).
///
/// The model is `p_calibrated = sigmoid(a * logit(p_raw) + b)`.
const EPS: f64 = 1e-6;

#[derive(Debug, Clone, Copy)]
pub struct PlattCalibration {
    pub a: f64,
    pub b: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct FitMetrics {
    pub logloss_before: f64,
    pub logloss_after: f64,
    pub brier_before: f64,
    pub brier_after: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct FitResult {
    pub calibration: PlattCalibration,
    pub metrics: FitMetrics,
}

fn clamp_prob(p: f64) -> f64 {
    p.clamp(EPS, 1.0 - EPS)
}

fn logit(p: f64) -> f64 {
    let p = clamp_prob(p);
    (p / (1.0 - p)).ln()
}

fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        let z = (-x).exp();
        1.0 / (1.0 + z)
    } else {
        let z = x.exp();
        z / (1.0 + z)
    }
}

fn logloss(p: f64, y: f64) -> f64 {
    let p = clamp_prob(p);
    -(y * p.ln() + (1.0 - y) * (1.0 - p).ln())
}

pub fn apply_platt(raw_prob: f64, model: PlattCalibration) -> f64 {
    let x = logit(raw_prob);
    sigmoid(model.a * x + model.b).clamp(0.0, 1.0)
}

pub fn fit_platt(
    samples: &[(f64, f64)],
    max_iters: usize,
    learning_rate: f64,
    l2: f64,
) -> Option<FitResult> {
    if samples.len() < 8 {
        return None;
    }
    let mut positives = 0usize;
    for (_, y) in samples {
        if *y > 0.5 {
            positives += 1;
        }
    }
    if positives == 0 || positives == samples.len() {
        return None;
    }

    let n = samples.len() as f64;
    let mut a = 1.0f64;
    let mut b = 0.0f64;

    for i in 0..max_iters.max(1) {
        let lr = learning_rate / (1.0 + 0.01 * i as f64);
        let mut grad_a = 0.0;
        let mut grad_b = 0.0;
        for (raw_p, y) in samples {
            let x = logit(*raw_p);
            let p = sigmoid(a * x + b);
            let err = p - *y;
            grad_a += err * x;
            grad_b += err;
        }
        grad_a = grad_a / n + l2 * a;
        grad_b /= n;
        a -= lr * grad_a;
        b -= lr * grad_b;
        if !a.is_finite() || !b.is_finite() {
            return None;
        }
    }

    let model = PlattCalibration { a, b };
    let mut ll_before = 0.0;
    let mut ll_after = 0.0;
    let mut br_before = 0.0;
    let mut br_after = 0.0;
    for (raw_p, y) in samples {
        let before = clamp_prob(*raw_p);
        let after = apply_platt(*raw_p, model);
        ll_before += logloss(before, *y);
        ll_after += logloss(after, *y);
        br_before += (before - *y).powi(2);
        br_after += (after - *y).powi(2);
    }
    let metrics = FitMetrics {
        logloss_before: ll_before / n,
        logloss_after: ll_after / n,
        brier_before: br_before / n,
        brier_after: br_after / n,
    };
    Some(FitResult {
        calibration: model,
        metrics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platt_fit_improves_distorted_synthetic_probabilities() {
        // Synthetic dataset where raw probabilities are overconfident.
        let mut samples = Vec::new();
        for i in 1..100 {
            let p_true = i as f64 / 100.0;
            let p_raw = ((p_true - 0.5) * 1.8 + 0.5).clamp(0.01, 0.99);
            let y = if p_true > 0.65 {
                1.0
            } else if p_true < 0.35 {
                0.0
            } else {
                (i % 2) as f64
            };
            samples.push((p_raw, y));
        }
        let fit = fit_platt(&samples, 500, 0.2, 1e-3).expect("fit should succeed");
        assert!(fit.metrics.logloss_after < fit.metrics.logloss_before);
    }

    #[test]
    fn apply_platt_bounds_output() {
        let m = PlattCalibration { a: 1.2, b: -0.1 };
        let p = apply_platt(0.999_999, m);
        assert!((0.0..=1.0).contains(&p));
    }
}
