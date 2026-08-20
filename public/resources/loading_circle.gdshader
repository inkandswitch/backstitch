shader_type canvas_item;
render_mode blend_mix, unshaded;

// Value 0.7 like default radius of loading circle
uniform float spinner_radius_factor : hint_range(0.1, 1.0, 0.01) = 0.7; 
// Dot color
uniform vec3 dot_color : source_color = vec3(1.0, 1.0, 1.0);
// Rotation speed
uniform float rotation_speed : hint_range(0.1, 5.0, 0.1) = 0.8;

void fragment() {
    vec2 uv_normalized = UV;
    vec2 centered_uv = uv_normalized - 0.5;

    float node_width_px = 1.0 / TEXTURE_PIXEL_SIZE.x;
    float node_height_px = 1.0 / TEXTURE_PIXEL_SIZE.y;

    float min_node_dim = min(node_width_px, node_height_px);
    
    vec2 u = centered_uv * (2.0 / min_node_dim);

    float i = 0.0;
    float d = 0.0;

    while (i < 10.0) {
        vec2 dot_pos = vec2(cos(0.63 * i), sin(0.63 * i)) * spinner_radius_factor; 
        
        float dot_radius = (spinner_radius_factor / 0.7) * 0.1 * mod(TIME * -rotation_speed - i * 0.1, 1.0);

        d += step(length(u - dot_pos) - dot_radius, 0.0);
        
        i++;
    }
    
    // Alpha channel based on 'd' to make transparent.
    COLOR = vec4(dot_color * d, clamp(d, 0.0, 1.0)); 
}
