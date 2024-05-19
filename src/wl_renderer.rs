use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use fps_counter::FPSCounter;

use gl::COLOR_BUFFER_BIT;
use khronos_egl::{ATTRIB_NONE, Surface};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output,
    delegate_registry, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};
use smithay_client_toolkit::output::OutputInfo;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::client::globals::{GlobalList, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_seat, wl_surface};
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::shell::wlr_layer::Anchor;
use wayland_egl::WlEglSurface;

use crate::egl::EGLState;
use crate::wallpaper::Wallpaper;

pub struct WLState {
    pub connection: Rc<Connection>,
    pub(crate) egl_state: Rc<EGLState>,
    pub(crate) _globals: GlobalList,
    pub(crate) event_queue: Rc<RefCell<EventQueue<WLState>>>,
    pub(crate) queue_handle: QueueHandle<WLState>,
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor_state: CompositorState,
    layer_shell: LayerShell,

    pub layers: HashMap<String, SimpleLayer>,
}

impl WLState {
    pub fn new() -> Self {
        let connection = Rc::new(Connection::connect_to_env().unwrap());
        let egl_state = Rc::new(EGLState::new(&connection));

        let (globals, event_queue) = registry_queue_init(&connection).unwrap();
        let queue_handle = event_queue.handle();

        WLState {
            connection,
            egl_state,
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &queue_handle),
            seat_state: SeatState::new(&globals, &queue_handle),
            compositor_state: CompositorState::bind(&globals, &queue_handle).expect("wl_compositor is not available"),
            layer_shell: LayerShell::bind(&globals, &queue_handle).expect("layer shell is not available"),
            _globals: globals,
            event_queue: Rc::new(RefCell::new(event_queue)),
            queue_handle,

            layers: HashMap::new(),
        }
    }

    pub(crate) fn loop_fn(&mut self) {
        loop {
            self.event_queue.clone().borrow_mut().blocking_dispatch(self).unwrap();

            if self.layers.values().any(|layer| layer.exit) {
                println!("Exiting");
                break;
            }
        }
    }

    pub fn setup_layer(&mut self, output: (&WlOutput, &OutputInfo)) -> &mut SimpleLayer {
        let surface = self.compositor_state.create_surface(&self.queue_handle);

        let output_size = output.1.logical_size.unwrap();

        let layer = self.layer_shell.create_layer_surface(
            &self.queue_handle,
            surface,
            Layer::Background,
            Some("waypaper_engine"),
            Some(output.0),
        );

        layer.set_exclusive_zone(-1); // -1 means we don't want our surface to be moved to accommodate for other surfaces
        layer.set_anchor(Anchor::BOTTOM | Anchor::TOP | Anchor::LEFT | Anchor::RIGHT); // All anchors means centered on screen
        layer.set_size(output_size.0 as u32, output_size.1 as u32); // We ask for the full size of the screen
        layer.set_keyboard_interactivity(KeyboardInteractivity::None); // No keyboard grabbing at all

        layer.commit();
        self.connection.roundtrip().unwrap(); // Block until the wayland server has processed everything

        let wl_egl_surface = WlEglSurface::new(
            layer.wl_surface().id(),
            output_size.0,
            output_size.1,
        ).unwrap();

        let egl_window_surface =
            unsafe {
                self.egl_state.egl.create_platform_window_surface(
                    self.egl_state.egl_display,
                    self.egl_state.config,
                    wl_egl_surface.ptr() as khronos_egl::NativeWindowType,
                    &[ATTRIB_NONE],
                )
            }.expect("Unable to create an EGL surface");

        layer.commit();
        self.connection.roundtrip().unwrap();

        self.egl_state.attach_context(egl_window_surface);

        let simple_layer = SimpleLayer {
            exit: false,
            first_configure: true,
            width: output_size.0 as u32,
            height: output_size.1 as u32,
            layer,
            //keyboard: None,
            //keyboard_focus: false,
            //pointer: None,
            //mpv_renderer,
            egl_state: self.egl_state.clone(),
            _wl_egl_surface: wl_egl_surface,
            egl_window_surface,
            output: (output.0.clone(), output.1.clone()),

            fps_counter: FPSCounter::new(),
            wallpaper: None,
        };

        let output_name = output.1.name.as_ref().unwrap().clone();
        self.layers.insert(output_name.clone(), simple_layer);

        self.layers.get_mut(&output_name).unwrap()
    }

    pub fn get_outputs(&mut self) -> OutputsList {
        self.event_queue.clone().borrow_mut().roundtrip(self).unwrap();

        OutputsList(self.output_state.outputs().map(|output| (output.clone(), self.output_state.info(&output).unwrap())).collect())
    }
}

pub struct OutputsList(HashMap<WlOutput, OutputInfo>);

impl OutputsList {
    pub fn print_outputs(&self) {
        for info in self.0.values() {
            let name = info.name.clone().expect("Output doesn't have a name");

            let current_mode = info.modes.iter().find(|mode| mode.current).expect("Couldn't find output current mode");
            let (width, height) = current_mode.dimensions;
            let refresh_rate = (current_mode.refresh_rate as f32 / 1000.0).ceil() as i32;
            let scale = info.scale_factor;

            println!("Outputs :");
            println!("\t- {:} : {}x{} - {}hz - {}", name, width, height, refresh_rate, scale);
        }
    }
}

impl Deref for OutputsList {
    type Target = HashMap<WlOutput, OutputInfo>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for OutputsList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct SimpleLayer {
    exit: bool,
    first_configure: bool,
    width: u32,
    height: u32,
    pub(crate) layer: LayerSurface,
    //keyboard: Option<wl_keyboard::WlKeyboard>,
    //keyboard_focus: bool,
    //pointer: Option<wl_pointer::WlPointer>,
    //mpv_renderer: MpvRenderer,
    egl_state: Rc<EGLState>,
    _wl_egl_surface: WlEglSurface,
    egl_window_surface: Surface,
    output: (WlOutput, OutputInfo),

    pub(crate) wallpaper: Option<Wallpaper>,
    fps_counter: FPSCounter,
}

impl CompositorHandler for WLState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {}

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {}

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.layers.values_mut().find(|layer| layer.layer.wl_surface() == surface).unwrap().draw(qh);
    }
}

impl OutputHandler for WLState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: WlOutput,
    ) {}

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
        // TODO resize wallpaper if output size or scale has changed
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: WlOutput,
    ) {
        if let Some(layer) = self.layers.values().find(|layer| layer.output.0 == output) {
            // TODO : test this
            self.layers.remove(&layer.output.1.name.clone().unwrap()).unwrap();
        }
    }
}

impl LayerShellHandler for WLState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        self.layers.values_mut().find(|l| l.layer == *layer).unwrap().exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        layer_surface: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        println!("New Size : {:?}", configure.new_size);

        let layer = self.layers.values_mut().find(|l| l.layer == *layer_surface).unwrap();

        // Size equal to zero means the compositor let us choose

        if configure.new_size.0 != 0 {
            layer.width = configure.new_size.0;
        }

        if configure.new_size.1 != 0 {
            layer.height = configure.new_size.1;
        }

        // Initiate the first draw.
        if layer.first_configure {
            layer.first_configure = false;
            layer.draw(qh);
        }
    }
}

impl SeatHandler for WLState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        _capability: Capability,
    ) {
        /*if capability == Capability::Keyboard && self.keyboard.is_none() {
            println!("Set keyboard capability");
            let keyboard =
                self.seat_state.get_keyboard(qh, &seat, None).expect("Failed to create keyboard");
            self.keyboard = Some(keyboard);
        }

        if capability == Capability::Pointer && self.pointer.is_none() {
            println!("Set pointer capability");
            let pointer = self.seat_state.get_pointer(qh, &seat).expect("Failed to create pointer");
            self.pointer = Some(pointer);
        }*/
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _capability: Capability,
    ) {
        /*if capability == Capability::Keyboard && self.keyboard.is_some() {
            println!("Unset keyboard capability");
            self.keyboard.take().unwrap().release();
        }

        if capability == Capability::Pointer && self.pointer.is_some() {
            println!("Unset pointer capability");
            self.pointer.take().unwrap().release();
        }*/
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl SimpleLayer {
    pub fn draw(&mut self, qh: &QueueHandle<WLState>) {
        let width = self.width;
        let height = self.height;

        // Attach the egl context to the current surface
        self.egl_state.attach_context(self.egl_window_surface);

        // Draw to the window:
        {
            unsafe {
                let clear_color = if let Some(wallpaper) = &self.wallpaper { wallpaper.clear_color() } else { (0.0, 0.0, 0.0) };

                gl::ClearColor(clear_color.0, clear_color.1, clear_color.2, 1.0);
                gl::Clear(COLOR_BUFFER_BIT);
                
                if let Some(wallpaper) = self.wallpaper.as_mut() {
                    wallpaper.render(self.width, self.height);
                }
            }
        }

        // Damage the entire window and swap buffers
        self.layer.wl_surface().damage_buffer(0, 0, width as i32, height as i32);
        self.egl_state.egl.swap_buffers(self.egl_state.egl_display, self.egl_window_surface).unwrap();

        // Now that buffers are swapped we can reset the egl context
        self.egl_state.detach_context();

        // Request our next frame
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());

        // Commit to present.
        self.layer.commit();

        let fps = self.fps_counter.tick();
        println!("Output {} : {} FPS", self.output.1.name.as_ref().unwrap(), fps);
    }
}

delegate_compositor!(WLState);
delegate_output!(WLState);
delegate_seat!(WLState);
delegate_layer!(WLState);
delegate_registry!(WLState);

impl ProvidesRegistryState for WLState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

