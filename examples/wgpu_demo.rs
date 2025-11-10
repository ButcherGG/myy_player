/// wgpu 独立窗口渲染演示
/// 运行: cargo run --example wgpu_demo

use winit::{
    event::*,
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

fn main() {
    env_logger::init();
    
    // 初始化 FFmpeg
    ffmpeg_next::init().expect("无法初始化 FFmpeg");
    
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("MYY Player - wgpu Demo")
        .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
        .build(&event_loop)
        .unwrap();
    
    // 创建渲染器（异步初始化）
    let renderer = pollster::block_on(myy_player::create_demo_renderer(&window));
    
    println!("✓ wgpu 渲染器已创建");
    println!("按 ESC 退出");
    
    event_loop.run(move |event, _, control_flow| {
        match event {
            Event::WindowEvent {
                ref event,
                window_id,
            } if window_id == window.id() => match event {
                WindowEvent::CloseRequested
                | WindowEvent::KeyboardInput {
                    input:
                        KeyboardInput {
                            state: ElementState::Pressed,
                            virtual_keycode: Some(VirtualKeyCode::Escape),
                            ..
                        },
                    ..
                } => *control_flow = ControlFlow::Exit,
                
                WindowEvent::Resized(physical_size) => {
                    println!("窗口大小改变: {:?}", physical_size);
                }
                _ => {}
            },
            Event::RedrawRequested(window_id) if window_id == window.id() => {
                // 这里将来渲染视频帧
                window.request_redraw();
            }
            Event::MainEventsCleared => {
                window.request_redraw();
            }
            _ => {}
        }
    });
}

