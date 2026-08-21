shader_type canvas_item;

// Fill color for the background
uniform vec4 fill_color : source_color = vec4(0.0, 0.0, 0.0, 0.0);

// Maximum number of rectangles we can handle
const int MAX_RECTANGLES = 1250;

// Array of rectangles (x, y, width, height)
uniform vec4 rectangles[MAX_RECTANGLES];

// Array of colors for each rectangle
uniform vec4 rectangles_color[MAX_RECTANGLES];

// Number of active rectangles
uniform int rectangle_count = 0;

// Maximum distance for the opacity transition
uniform float max_distance = 0.0025;

float distance_to_rectangle(vec2 point, vec4 rect) {
    // Calculate the closest point on the rectangle to the given point
    vec2 closest_point;

    // Clamp the point to the rectangle's bounds
    closest_point.x = max(rect.x, min(point.x, rect.x + rect.z));
    closest_point.y = max(rect.y, min(point.y, rect.y + rect.w));

    // Return the distance to the closest point
    return distance(point, closest_point);
}

void fragment() {
    // Default to the fill color
    vec4 color = fill_color;

    // Track the minimum distance to any rectangle
    float min_distance = max_distance;
    
    // Track the index of the closest rectangle
    int closest_rect_index = -1;

    for (int i = 0; i < rectangle_count; i++) {
        // Calculate distance to this rectangle
        float dist = distance_to_rectangle(UV, rectangles[i]);
        
        if (dist < min_distance) {
            min_distance = dist;
            closest_rect_index = i;
        }
    }

    if (closest_rect_index >= 0 && min_distance < max_distance) {
        // Calculate opacity based on distance
        // The closer to the rectangle, the more transparent


        if (min_distance > 0.0015) {
            color = rectangles_color[closest_rect_index];
        } else {
            color = vec4(0.0, 0.0, 0.0, 0.0);        
        }
    }

    COLOR = color;
}