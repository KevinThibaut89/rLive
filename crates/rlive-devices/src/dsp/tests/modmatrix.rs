use crate::dsp::modmatrix::*;

fn add(source: usize, dest: usize, amount: f32) -> Option<Route> {
    Some(Route {
        source,
        dest,
        amount,
        kind: RouteKind::Add,
        via: None,
    })
}

fn mul(source: usize, dest: usize, amount: f32) -> Option<Route> {
    Some(Route {
        source,
        dest,
        amount,
        kind: RouteKind::Mul,
        via: None,
    })
}

#[test]
fn additive_routes_sum() {
    let mut m = ModMatrix::new(2, 2, 4);
    m.set_route(0, add(0, 0, 2.0));
    m.set_route(1, add(1, 0, -3.0));
    let mut bus = m.bus();
    m.evaluate(&[0.5, 1.0], &mut bus);
    assert_eq!(bus.offset(0), 1.0 - 3.0);
    assert_eq!(bus.apply(0, 10.0), 8.0);
    assert_eq!(
        bus.apply(1, 10.0),
        10.0,
        "an untouched destination is unchanged"
    );
}

#[test]
fn multiplicative_routes_multiply() {
    let mut m = ModMatrix::new(2, 1, 4);
    m.set_route(0, mul(0, 0, 1.0));
    m.set_route(1, mul(1, 0, 0.5));
    let mut bus = m.bus();
    m.evaluate(&[0.5, 0.0], &mut bus);
    // 0.5 (full depth, source at half) x 0.5 (half depth, source at zero).
    assert_eq!(bus.gain(0), 0.25);
}

#[test]
fn zero_amount_leaves_the_destination_alone() {
    let mut m = ModMatrix::new(1, 1, 1);
    m.set_route(0, mul(0, 0, 0.0));
    let mut bus = m.bus();
    m.evaluate(&[0.3], &mut bus);
    assert_eq!(bus.gain(0), 1.0);
}

#[test]
fn additive_comes_before_multiplicative() {
    let mut m = ModMatrix::new(2, 1, 2);
    m.set_route(0, add(0, 0, 1.0));
    m.set_route(1, mul(1, 0, 1.0));
    let mut bus = m.bus();
    m.evaluate(&[1.0, 0.5], &mut bus);
    assert_eq!(bus.apply(0, 1.0), (1.0 + 1.0) * 0.5);
}

#[test]
fn a_via_source_scales_the_amount() {
    let mut m = ModMatrix::new(2, 1, 1);
    m.set_route(
        0,
        Some(Route {
            source: 0,
            dest: 0,
            amount: 2.0,
            kind: RouteKind::Add,
            via: Some(1),
        }),
    );
    let mut bus = m.bus();
    m.evaluate(&[1.0, 0.25], &mut bus);
    assert_eq!(bus.offset(0), 0.5);
}

#[test]
fn multiplicative_never_goes_negative() {
    let mut m = ModMatrix::new(1, 1, 1);
    m.set_route(0, mul(0, 0, 1.0));
    let mut bus = m.bus();
    m.evaluate(&[-2.0], &mut bus);
    assert_eq!(bus.gain(0), 0.0);
}

#[test]
fn invalid_routes_are_ignored_and_bad_values_are_contained() {
    let mut m = ModMatrix::new(1, 1, 1);
    m.set_route(0, add(5, 0, 1.0));
    assert!(m.routes()[0].is_none());
    m.set_route(9, add(0, 0, 1.0));

    m.set_route(0, add(0, 0, 1.0));
    let mut bus = m.bus();
    m.evaluate(&[f32::NAN], &mut bus);
    assert_eq!(bus.apply(0, 2.0), 2.0);
    m.evaluate(&[f32::INFINITY], &mut bus);
    assert_eq!(bus.offset(0), 0.0);
}
