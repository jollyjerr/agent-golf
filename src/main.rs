use std::collections::VecDeque;
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{
        Block, Borders, Paragraph,
        canvas::{self, Canvas, Circle},
    },
};

// ---- Constants ----

const GRID: usize = 22;
const Z_SCALE: f64 = 0.7;
const DOMAIN: f64 = 10.0;
const GRAVITY: f64 = 0.4;
const DAMPING: f64 = 0.90;
const DT: f64 = 0.06;
const EPS: f64 = 0.05;
const PUTT_POWER_STEP: f64 = 0.3;
const AIM_STEP: f64 = 0.08;
const MORPH_TICKS: f64 = 60.0;
const STUCK_TICKS: u32 = 20;
const TRAIL_LEN: usize = 60;

// ---- Surface math ----

#[derive(Clone)]
struct Gaussian {
    cx: f64,
    cy: f64,
    amplitude: f64,
    sigma: f64,
}

impl Gaussian {
    fn eval(&self, x: f64, y: f64) -> f64 {
        let dx = x - self.cx;
        let dy = y - self.cy;
        self.amplitude * (-(dx * dx + dy * dy) / (2.0 * self.sigma * self.sigma)).exp()
    }

    fn lerp(&self, other: &Gaussian, t: f64) -> Gaussian {
        Gaussian {
            cx: self.cx + (other.cx - self.cx) * t,
            cy: self.cy + (other.cy - self.cy) * t,
            amplitude: self.amplitude + (other.amplitude - self.amplitude) * t,
            sigma: self.sigma + (other.sigma - self.sigma) * t,
        }
    }
}

fn surface(x: f64, y: f64, gaussians: &[Gaussian]) -> f64 {
    gaussians.iter().map(|g| g.eval(x, y)).sum()
}

fn surface_grad(x: f64, y: f64, gaussians: &[Gaussian]) -> (f64, f64) {
    let gx = (surface(x + EPS, y, gaussians) - surface(x - EPS, y, gaussians)) / (2.0 * EPS);
    let gy = (surface(x, y + EPS, gaussians) - surface(x, y - EPS, gaussians)) / (2.0 * EPS);
    (gx, gy)
}

fn height_to_color(h: f64) -> Color {
    if h < -3.5 {
        Color::Rgb(255, 50, 255)
    } else if h < -2.0 {
        Color::Rgb(180, 50, 220)
    } else if h < -0.8 {
        Color::Rgb(50, 80, 220)
    } else if h < 0.2 {
        Color::Rgb(30, 180, 180)
    } else if h < 1.2 {
        Color::Rgb(50, 200, 80)
    } else if h < 2.2 {
        Color::Rgb(220, 200, 50)
    } else {
        Color::Rgb(220, 80, 50)
    }
}

// ---- Isometric projection ----
// Canvas y-axis points up, so positive z (hills) must increase cy.

fn project(x: f64, y: f64, z: f64) -> (f64, f64) {
    let cx = (x - y) * 0.866;
    let cy = (x + y) * 0.45 + z * Z_SCALE;
    (cx, cy)
}

// ---- Game state ----

#[derive(PartialEq, Clone)]
enum GameState {
    Aiming,
    Rolling,
    Stuck,
    Reshaping,
    InHole,
}

// ---- Landscape configs ----

fn gaussians_before() -> Vec<Gaussian> {
    vec![
        // Large hills blocking the path
        Gaussian {
            cx: 4.0,
            cy: 5.0,
            amplitude: 3.2,
            sigma: 1.4,
        },
        Gaussian {
            cx: 6.5,
            cy: 3.0,
            amplitude: 2.5,
            sigma: 1.0,
        },
        Gaussian {
            cx: 2.5,
            cy: 7.5,
            amplitude: 2.8,
            sigma: 1.1,
        },
        // Medium bumps / ridges
        Gaussian {
            cx: 5.0,
            cy: 8.0,
            amplitude: 1.8,
            sigma: 0.9,
        },
        Gaussian {
            cx: 7.5,
            cy: 6.5,
            amplitude: 1.5,
            sigma: 0.8,
        },
        Gaussian {
            cx: 3.0,
            cy: 1.5,
            amplitude: 1.6,
            sigma: 0.7,
        },
        Gaussian {
            cx: 8.5,
            cy: 2.0,
            amplitude: 1.4,
            sigma: 0.8,
        },
        // Local minimum traps
        Gaussian {
            cx: 3.0,
            cy: 3.5,
            amplitude: -1.8,
            sigma: 0.7,
        },
        Gaussian {
            cx: 6.0,
            cy: 6.5,
            amplitude: -1.4,
            sigma: 0.6,
        },
        Gaussian {
            cx: 7.5,
            cy: 4.5,
            amplitude: -1.2,
            sigma: 0.5,
        },
        // The hole — narrow, hard to reach
        Gaussian {
            cx: 8.0,
            cy: 7.5,
            amplitude: -5.0,
            sigma: 0.55,
        },
    ]
}

fn gaussians_after() -> Vec<Gaussian> {
    vec![
        // Hills flattened
        Gaussian {
            cx: 4.0,
            cy: 5.0,
            amplitude: 0.5,
            sigma: 1.4,
        },
        Gaussian {
            cx: 6.5,
            cy: 3.0,
            amplitude: 0.4,
            sigma: 1.0,
        },
        Gaussian {
            cx: 2.5,
            cy: 7.5,
            amplitude: 0.5,
            sigma: 1.1,
        },
        Gaussian {
            cx: 5.0,
            cy: 8.0,
            amplitude: 0.3,
            sigma: 0.9,
        },
        Gaussian {
            cx: 7.5,
            cy: 6.5,
            amplitude: 0.3,
            sigma: 0.8,
        },
        Gaussian {
            cx: 3.0,
            cy: 1.5,
            amplitude: 0.3,
            sigma: 0.7,
        },
        Gaussian {
            cx: 8.5,
            cy: 2.0,
            amplitude: 0.3,
            sigma: 0.8,
        },
        // Traps mostly filled
        Gaussian {
            cx: 3.0,
            cy: 3.5,
            amplitude: -0.2,
            sigma: 0.7,
        },
        Gaussian {
            cx: 6.0,
            cy: 6.5,
            amplitude: -0.2,
            sigma: 0.6,
        },
        Gaussian {
            cx: 7.5,
            cy: 4.5,
            amplitude: -0.1,
            sigma: 0.5,
        },
        // Hole widens and deepens — clear basin of attraction
        Gaussian {
            cx: 8.0,
            cy: 7.5,
            amplitude: -6.0,
            sigma: 1.2,
        },
    ]
}

// ---- App ----

struct App {
    state: GameState,
    prev_state: GameState,

    gaussians_before: Vec<Gaussian>,
    gaussians_after: Vec<Gaussian>,
    gaussians_current: Vec<Gaussian>,
    morph_t: f64,

    ball_x: f64,
    ball_y: f64,
    vel_x: f64,
    vel_y: f64,
    stuck_counter: u32,

    aim_angle: f64,
    putt_power: f64,

    hole_x: f64,
    hole_y: f64,
    hole_radius: f64,

    trail: VecDeque<(f64, f64)>,
    putt_count: u32,
    reshaped: bool,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        let gb = gaussians_before();
        let ga = gaussians_after();
        let gc = gb.clone();
        App {
            state: GameState::Aiming,
            prev_state: GameState::Aiming,
            gaussians_before: gb,
            gaussians_after: ga,
            gaussians_current: gc,
            morph_t: 0.0,
            ball_x: 1.5,
            ball_y: 1.5,
            vel_x: 0.0,
            vel_y: 0.0,
            stuck_counter: 0,
            aim_angle: std::f64::consts::FRAC_PI_4,
            putt_power: 2.5,
            hole_x: 8.0,
            hole_y: 7.5,
            hole_radius: 0.45,
            trail: VecDeque::new(),
            putt_count: 0,
            reshaped: false,
            should_quit: false,
        }
    }

    fn current_gaussians(&self) -> &[Gaussian] {
        &self.gaussians_current
    }

    fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let tick = Duration::from_millis(33);
        loop {
            terminal.draw(|f| self.render(f))?;
            if event::poll(tick)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.handle_key(key.code);
                    }
                }
            }
            self.update();
            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Left => {
                if self.state == GameState::Aiming {
                    self.aim_angle -= AIM_STEP;
                }
            }
            KeyCode::Right => {
                if self.state == GameState::Aiming {
                    self.aim_angle += AIM_STEP;
                }
            }
            KeyCode::Up => {
                if self.state == GameState::Aiming {
                    self.putt_power = (self.putt_power + PUTT_POWER_STEP).min(6.0);
                }
            }
            KeyCode::Down => {
                if self.state == GameState::Aiming {
                    self.putt_power = (self.putt_power - PUTT_POWER_STEP).max(0.5);
                }
            }
            KeyCode::Char(' ') => {
                if self.state == GameState::Aiming {
                    self.vel_x = self.aim_angle.cos() * self.putt_power;
                    self.vel_y = self.aim_angle.sin() * self.putt_power;
                    self.trail.clear();
                    self.putt_count += 1;
                    self.state = GameState::Rolling;
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if self.state == GameState::Rolling {
                    self.vel_x = 0.0;
                    self.vel_y = 0.0;
                    self.state = GameState::Aiming;
                }
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if matches!(
                    self.state,
                    GameState::Stuck | GameState::Aiming | GameState::Rolling
                ) {
                    self.prev_state = self.state.clone();
                    self.state = GameState::Reshaping;
                    self.morph_t = 0.0;
                    self.reshaped = true;
                }
            }
            KeyCode::Enter => {
                if self.state == GameState::Stuck || self.state == GameState::InHole {
                    self.reset_ball();
                }
            }
            _ => {}
        }
    }

    fn reset_ball(&mut self) {
        self.ball_x = 1.5;
        self.ball_y = 1.5;
        self.vel_x = 0.0;
        self.vel_y = 0.0;
        self.stuck_counter = 0;
        self.trail.clear();
        self.state = GameState::Aiming;
    }

    fn update(&mut self) {
        match self.state.clone() {
            GameState::Rolling => self.update_physics(),
            GameState::Reshaping => self.update_morph(),
            _ => {}
        }
    }

    fn update_physics(&mut self) {
        let g = self.current_gaussians();
        let (gx, gy) = surface_grad(self.ball_x, self.ball_y, g);

        self.vel_x = self.vel_x * DAMPING - gx * GRAVITY * DT;
        self.vel_y = self.vel_y * DAMPING - gy * GRAVITY * DT;

        self.ball_x = (self.ball_x + self.vel_x * DT).clamp(0.1, 9.9);
        self.ball_y = (self.ball_y + self.vel_y * DT).clamp(0.1, 9.9);

        self.trail.push_back((self.ball_x, self.ball_y));
        if self.trail.len() > TRAIL_LEN {
            self.trail.pop_front();
        }

        let speed = (self.vel_x * self.vel_x + self.vel_y * self.vel_y).sqrt();

        let dx = self.ball_x - self.hole_x;
        let dy = self.ball_y - self.hole_y;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < self.hole_radius {
            self.state = GameState::InHole;
            return;
        }

        if speed < 0.012 {
            self.stuck_counter += 1;
            if self.stuck_counter >= STUCK_TICKS {
                self.state = GameState::Stuck;
            }
        } else {
            self.stuck_counter = 0;
        }
    }

    fn update_morph(&mut self) {
        self.morph_t = (self.morph_t + 1.0 / MORPH_TICKS).min(1.0);
        self.gaussians_current = self
            .gaussians_before
            .iter()
            .zip(self.gaussians_after.iter())
            .map(|(b, a)| b.lerp(a, self.morph_t))
            .collect();

        if self.morph_t >= 1.0 {
            self.state = GameState::Aiming;
        }
    }

    // ---- Rendering ----

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(area);

        // Title
        let title_text = match self.state {
            GameState::Reshaping => " P(X|C) → P(X|C')  [Context Change]",
            GameState::InHole => " ⛳ Global Minimum Reached — Valid Code Found!",
            _ => " E(x) = Σ aᵢ·exp(−‖x−cᵢ‖²/2σᵢ²)   |   Agent Golf",
        };
        let title_color = match self.state {
            GameState::Reshaping => Color::Yellow,
            GameState::InHole => Color::Green,
            _ => Color::Cyan,
        };
        let title = Paragraph::new(title_text).style(Style::default().fg(title_color));
        frame.render_widget(title, chunks[0]);

        // Canvas
        self.render_canvas(frame, chunks[1]);

        // Footer
        let state_msg: &str = match self.state {
            GameState::Aiming => "Aim and putt",
            GameState::Rolling => "Sampling inference in progress...",
            GameState::Stuck => "Stuck in local minimum — add context with [r] to reshape",
            GameState::Reshaping => "Reshaping energy landscape...",
            GameState::InHole => "Global minimum found! Press [Enter] to reset",
        };
        let controls = if self.state == GameState::Aiming {
            format!(
                "  ←/→ Aim  ↑/↓ Power: {:.1}  [Space] Putt  [r] Reshape  [q] Quit  |  {}",
                self.putt_power, state_msg
            )
        } else {
            format!(
                "  [s] Stop  [r] Reshape  [Enter] Reset  [q] Quit  |  Putt #{} | {}",
                self.putt_count, state_msg
            )
        };
        let footer = Paragraph::new(controls)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP));
        frame.render_widget(footer, chunks[2]);
    }

    fn render_canvas(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let gaussians = self.current_gaussians();

        // Pre-compute grid
        let mut pts: Vec<Vec<(f64, f64)>> = vec![vec![(0.0, 0.0); GRID]; GRID];
        let mut heights: Vec<Vec<f64>> = vec![vec![0.0; GRID]; GRID];
        for i in 0..GRID {
            for j in 0..GRID {
                let x = i as f64 * DOMAIN / (GRID - 1) as f64;
                let y = j as f64 * DOMAIN / (GRID - 1) as f64;
                let z = surface(x, y, gaussians);
                heights[i][j] = z;
                pts[i][j] = project(x, y, z);
            }
        }

        // Ball screen pos
        let bz = surface(self.ball_x, self.ball_y, gaussians);
        let (bsx, bsy) = project(self.ball_x, self.ball_y, bz);

        // Aim line endpoint
        let aim_len = self.putt_power * 0.4;
        let ax = self.ball_x + self.aim_angle.cos() * aim_len;
        let ay = self.ball_y + self.aim_angle.sin() * aim_len;
        let az = surface(ax.clamp(0.1, 9.9), ay.clamp(0.1, 9.9), gaussians);
        let (asx, asy) = project(ax, ay, az);

        // Hole screen pos
        let hz = surface(self.hole_x, self.hole_y, gaussians);
        let (hsx, hsy) = project(self.hole_x, self.hole_y, hz);

        // Trail screen positions with age
        let trail_pts: Vec<((f64, f64), usize)> = self
            .trail
            .iter()
            .enumerate()
            .map(|(i, &(tx, ty))| {
                let tz = surface(tx, ty, gaussians);
                (project(tx, ty, tz), i)
            })
            .collect();

        let morph_t = self.morph_t;
        let state = self.state.clone();
        let putt_count = self.putt_count;

        let canvas = Canvas::default()
            .block(Block::default().borders(Borders::ALL))
            .x_bounds([-9.5, 9.5])
            .y_bounds([-5.0, 14.0])
            .marker(ratatui::symbols::Marker::Braille)
            .paint(move |ctx| {
                // Draw surface wireframe back-to-front (high i+j first)
                for sum in (0..(2 * GRID - 1)).rev() {
                    for i in 0..GRID {
                        let j = if sum >= i { sum - i } else { continue };
                        if j >= GRID {
                            continue;
                        }

                        let (sx, sy) = pts[i][j];
                        let h = heights[i][j];
                        let col = height_to_color(h);

                        if i + 1 < GRID {
                            let (nx, ny) = pts[i + 1][j];
                            ctx.draw(&canvas::Line::new(sx, sy, nx, ny, col));
                        }
                        if j + 1 < GRID {
                            let (nx, ny) = pts[i][j + 1];
                            ctx.draw(&canvas::Line::new(sx, sy, nx, ny, col));
                        }
                    }
                }

                ctx.layer();

                // Hole
                ctx.draw(&Circle {
                    x: hsx,
                    y: hsy,
                    radius: 0.35,
                    color: Color::Rgb(255, 80, 255),
                });

                // Label: hole
                ctx.print(hsx + 0.4, hsy + 0.3, "Desired Artifacts");

                ctx.layer();

                // Trail (fading)
                let trail_len = trail_pts.len();
                for &((tx, ty), age) in &trail_pts {
                    let brightness = (age as f64 / trail_len as f64 * 200.0) as u8 + 55;
                    ctx.draw(&Circle {
                        x: tx,
                        y: ty,
                        radius: 0.07,
                        color: Color::Rgb(brightness, brightness, brightness),
                    });
                }

                ctx.layer();

                // Ball
                let ball_color = if state == GameState::InHole {
                    Color::Rgb(80, 255, 80)
                } else {
                    Color::White
                };
                ctx.draw(&Circle {
                    x: bsx,
                    y: bsy,
                    radius: 0.22,
                    color: ball_color,
                });
                ctx.print(bsx + 0.3, bsy + 0.2, "⬤ LLM Output");

                // Aim line
                if state == GameState::Aiming {
                    ctx.draw(&canvas::Line::new(bsx, bsy, asx, asy, Color::Yellow));
                }

                // Reshape overlay label
                if state == GameState::Reshaping {
                    ctx.print(-7.0, 10.5, format!("Reshaping: {:.0}%", morph_t * 100.0));
                }

                // Putt counter label
                ctx.print(-8.5, 11.2, format!("Putt #{}", putt_count));
            });

        frame.render_widget(canvas, area);
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut terminal = ratatui::init();
    let result = App::new().run(&mut terminal);
    ratatui::restore();
    result
}
