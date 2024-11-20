use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_hal::{gpio::GpioPin, spi};
// use esp_println::println;

use crate::{
    hardware::{Accelerometer, Motor},
    logging::log,
    math, mk_static,
    shared_code::controller::{ControllerState, Button},
};

pub async fn control_logic(
    spawner: Spawner,
    controllers: &'static Mutex<NoopRawMutex, (ControllerState, ControllerState)>,
    armed: &'static Mutex<NoopRawMutex, bool>,
    mut left_motor: Motor<GpioPin<21>>,
    mut right_motor: Motor<GpioPin<5>>,
    accelerometer: Accelerometer,
) -> ! {
    log!(InitializingMotors);

    left_motor.start_duty_fade(0, 100, 1000).unwrap();
    right_motor.start_duty_fade(0, 100, 1000).unwrap();
    while left_motor.is_fade_running() || right_motor.is_fade_running() {}
    left_motor.start_duty_fade(100, 0, 1000).unwrap();
    right_motor.start_duty_fade(100, 0, 1000).unwrap();
    while left_motor.is_fade_running() || right_motor.is_fade_running() {}

    log!(MotorsInitialized);

    let state_vector = &*mk_static!(
        Mutex<NoopRawMutex, StateVector>, Mutex::new(StateVector {
        time: Instant::now(),
        theta: 0.0,
        omega: 0.0,
    }));

    spawner
        .spawn(accelerometer_data(state_vector, accelerometer))
        .ok();
    motor_control(controllers, armed, left_motor, right_motor, state_vector).await;
}

#[embassy_executor::task]
async fn accelerometer_data(
    state_vector: &'static Mutex<NoopRawMutex, StateVector>,
    mut accelerometer: Accelerometer,
) -> ! {
    const ACCELEROMETER_POSITION: f32 = 6.0 * 1.0e-3; // 6mm

    let period = Duration::from_hz(800);

    let mut offset_calibration = 0.;
    const SAMPLES: usize = 800 * 5;

    for _ in 0..SAMPLES {
        Timer::after(period).await;

        let Ok(data) = accelerometer.read_all().await else {
            continue;
        };

        offset_calibration += data.y;
    }
    let offset = offset_calibration / SAMPLES as f32;

    loop {
        Timer::after(period).await;

        let Ok(data) = accelerometer.read_all().await else {
            continue;
        };
        let time = Instant::now();
        // println!("{}", data);

        // a = r * omega^2
        // omega = 1 / sqrt(r / a)
        // println!("{}", data.y - offset);
        let omega = math::sqrt(math::abs(data.y - offset) / ACCELEROMETER_POSITION);

        let mut state = state_vector.lock().await;
        let average_omega = (state.omega + omega) * 0.5;
        let dt = time - state.time;
        let dtheta = average_omega * f32_seconds(dt);
        let theta = state.theta + dtheta;
        *state = StateVector { time, theta, omega }
    }
}

async fn motor_control(
    controllers: &Mutex<NoopRawMutex, (ControllerState, ControllerState)>,
    armed: &Mutex<NoopRawMutex, bool>,
    mut left_motor: Motor<GpioPin<21>>,
    mut right_motor: Motor<GpioPin<5>>,
    state_vector: &Mutex<NoopRawMutex, StateVector>,
) -> ! {
    let dt = Duration::from_hz(2000);
    let mut ticker = Ticker::every(dt);

    let spin_power = 60.;
    let move_power = 20.;

    loop {
        ticker.next().await;

        if !*armed.lock().await {
            left_motor.set_duty(50).unwrap();
            right_motor.set_duty(50).unwrap();
            continue;
        }

        let (primary_controller, _secondary_controller) = { *controllers.lock().await };
        let state = { state_vector.lock().await.predict(Instant::now()) };

        let mut left_power = 0.;
        let mut right_power = 0.;

        if primary_controller.get(Button::LeftBumper) {
            left_power += spin_power;
            right_power += spin_power;
        }

        let angle_correction = f32_seconds(dt) * state.omega;
        let stick_x = primary_controller.left_stick.get_x();
        let stick_y = primary_controller.left_stick.get_y();
        let magnitude = math::sqrt(stick_x * stick_x + stick_y * stick_y);
        if magnitude > 0.1 {
            let controller_angle = math::atan2(
                primary_controller.left_stick.get_y(),
                primary_controller.left_stick.get_x(),
            );
            let angle_error = state.theta - controller_angle;
            let movement = move_power * math::cos(angle_error + angle_correction);
            left_power -= movement;
            right_power += movement;
        }

        // println!("Angle: {}", math::rad2deg(state.theta));

        // println!("{:.8}\t{:.8}", left_power, right_power);
        left_motor.set_power(left_power).unwrap();
        right_motor.set_power(right_power).unwrap();
    }
}

/*
    poses = [Pose(0, 0, 0)]
    step = 0.01
    move_weight = 0.5
    angular_velocity = 10
    motor_speed = angular_velocity * 0.16 / 2
    angle_offset = angular_velocity * step
    print(np.rad2deg(angle_offset))
    ts = np.arange(0, 10, step)
    for t in ts:
        pose = poses[-1]
        theta = pose.theta
        movement = move_weight * np.cos(theta + angle_offset)
        robot_simulation_step(poses, -motor_speed + movement, motor_speed + movement, step)
    draw_robot_path(poses)
*/

fn f32_seconds(duration: Duration) -> f32 {
    const SECONDS_PER_MICROSECOND: f32 = 1.0e-6;
    duration.as_micros() as f32 * SECONDS_PER_MICROSECOND
}

#[derive(Debug, Clone, Copy)]
struct StateVector {
    time: Instant,
    theta: f32, // radians
    omega: f32, // radians per second
}

impl StateVector {
    fn predict(&self, current_time: Instant) -> Self {
        let dt = current_time - self.time;
        let theta = self.theta + self.omega * f32_seconds(dt);
        let omega = self.omega;
        Self {
            time: current_time,
            theta,
            omega,
        }
    }
}

// leftChannel.start_duty_fade(0, 100, 1000).unwrap();
// rightChannel.start_duty_fade(0, 100, 1000).unwrap();
// while leftChannel.is_duty_fade_running() || rightChannel.is_duty_fade_running() {
// }
// leftChannel.start_duty_fade(100, 0, 1000).unwrap();
// rightChannel.start_duty_fade(100, 0, 1000).unwrap();
// while leftChannel.is_duty_fade_running() || rightChannel.is_duty_fade_running() {
// }

// loop {
//     leftChannel.start_duty_fade(60, 100, 1000).unwrap();
//     rightChannel.start_duty_fade(60, 100, 1000).unwrap();
//     while leftChannel.is_duty_fade_running() || rightChannel.is_duty_fade_running() {
//     }
//     leftChannel.start_duty_fade(100, 51, 1000).unwrap();
//     rightChannel.start_duty_fade(100, 51, 1000).unwrap();
//     while leftChannel.is_duty_fade_running() || rightChannel.is_duty_fade_running() {
//     }

// }
