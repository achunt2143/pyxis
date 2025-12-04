#ifndef GECKOOS_GECKO_H
#define GECKOOS_GECKO_H

#ifdef __cplusplus
extern "C" {
#endif

// Gecko initialization
int gecko_init(const char* profile_dir);
void gecko_shutdown();

// Surface management
int gecko_create_surface(int width, int height);
void gecko_destroy_surface(int surface_id);

// Rendering
void gecko_render_frame(int surface_id);
void gecko_get_rendered_buffer(int surface_id, uint8_t** buffer, int* size);

// JavaScript execution
int gecko_execute_script(const char* script);
void gecko_set_dom_property(const char* selector, const char* property, const char* value);

// Events
void gecko_handle_keyboard_event(int keycode, int modifiers, int pressed);
void gecko_handle_mouse_event(int x, int y, int button, int pressed);

#ifdef __cplusplus
}
#endif

#endif // GECKOOS_GECKO_H
