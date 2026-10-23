use crate::{Shared, document, element, model, render, window};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{Element, Event, HtmlElement};

pub const WINDOW_THRESHOLD: usize = 256;
// Keep these heights in sync with the fixed cells in styles/vm.css.
const ROW_HEIGHT: usize = 54;
const HEADER_HEIGHT: usize = 32;
const OVERSCAN: usize = 8;

pub fn cache(app: &Shared) {
    let mut state = app.borrow_mut();
    state.vm_rows.clear();
    state.vm_window = None;
    let Some(body) = element("vm-rows") else {
        state.vm_indices.clear();
        state.vm_scroll_top = 0;
        return;
    };
    state.vm_rows = vec![None; state.vms.len()];
    if let Ok(rows) = body.query_selector_all("tr[data-id]") {
        for i in 0..rows.length() {
            let Some(row) = rows
                .item(i)
                .and_then(|row| row.dyn_into::<HtmlElement>().ok())
            else {
                continue;
            };
            let id = row.get_attribute("data-id").unwrap_or_default();
            if let Some(index) = state.vms.iter().position(|vm| vm.id == id) {
                state.vm_rows[index] = Some(row);
            }
        }
    }
    drop(state);
    update_filter(app, false);
}

pub fn filter(app: &Shared) {
    update_filter(app, true);
}

fn update_filter(app: &Shared, reset_scroll: bool) {
    if element("vm-rows").is_none() {
        return;
    }
    let mut state = app.borrow_mut();
    let query = model::lowercase(state.query.trim());
    let indices: Vec<usize> = state
        .vms
        .iter()
        .enumerate()
        .filter_map(|(i, vm)| {
            ((state.filter == "all" || state.filter == vm.state)
                && (query.is_empty()
                    || state
                        .search_index
                        .get(i)
                        .is_some_and(|index| index.contains(&query))))
            .then_some(i)
        })
        .collect();
    let reset = state.vm_indices != indices;
    if reset {
        state.vm_indices = indices;
        state.vm_window = None;
    }
    drop(state);
    render_window(app, reset && reset_scroll);
}

fn spacer(height: usize) -> Option<Element> {
    let row = document().create_element("tr").ok()?;
    row.set_class_name("vm-spacer");
    let _ = row.set_attribute("role", "presentation");
    let _ = row.set_attribute("aria-hidden", "true");
    row.set_inner_html(&format!(
        "<td colspan=\"6\" style=\"height:{height}px\"></td>"
    ));
    Some(row)
}

fn render_window(app: &Shared, reset: bool) {
    let Some(current) = element("vm-rows") else {
        return;
    };
    let Some(port) = element("vm-scroll").and_then(|port| port.dyn_into::<HtmlElement>().ok())
    else {
        return;
    };
    let mut state = app.borrow_mut();
    if state.rendering {
        return;
    }
    let count = state.vm_indices.len();
    let height = port.client_height().max(ROW_HEIGHT as i32) as usize;
    let max_top = (count * ROW_HEIGHT + HEADER_HEIGHT).saturating_sub(height);
    let top = if reset {
        0
    } else {
        (state.vm_scroll_top.max(0) as usize).min(max_top)
    };
    state.vm_scroll_top = top as i32;
    let (start, end) = if state.vms.len() > WINDOW_THRESHOLD {
        let start = (top.saturating_sub(HEADER_HEIGHT) / ROW_HEIGHT)
            .saturating_sub(OVERSCAN)
            .min(count);
        (
            start,
            (start + height.div_ceil(ROW_HEIGHT) + 2 * OVERSCAN).min(count),
        )
    } else {
        (0, count)
    };
    let _ = current.set_attribute("data-visible-count", &count.to_string());
    if let Some(table) = current.parent_element() {
        let _ = table.set_attribute("aria-rowcount", &(count + 1).to_string());
        let _ = table.set_attribute("data-visible-count", &count.to_string());
    }
    if let Some(empty) = element("vm-empty").and_then(|empty| empty.dyn_into::<HtmlElement>().ok())
    {
        empty.set_hidden(count != 0);
    }
    if state.vm_window == Some((start, end)) {
        return;
    }
    let Ok(body) = document().create_element("tbody") else {
        return;
    };
    body.set_id("vm-rows");
    let _ = body.set_attribute("data-visible-count", &count.to_string());
    let mut rows = Vec::with_capacity(end - start);
    for position in start..end {
        let index = state.vm_indices[position];
        if state.vm_rows[index].is_none() {
            let Ok(holder) = document().create_element("tbody") else {
                return;
            };
            holder.set_inner_html(&render::vm_row(&state, &state.vms[index]));
            let Some(row) = holder
                .first_element_child()
                .and_then(|row| row.dyn_into::<HtmlElement>().ok())
            else {
                return;
            };
            state.vm_rows[index] = Some(row);
        }
        if let Some(row) = &state.vm_rows[index] {
            rows.push((position, row.clone()));
        }
    }
    state.vm_window = Some((start, end));
    state.rendering = true;
    drop(state);

    // Moving a focused row can dispatch blur synchronously. Release the RefCell first.
    let active = document()
        .active_element()
        .and_then(|active| active.dyn_into::<HtmlElement>().ok());
    if start > 0
        && let Some(top) = spacer(start * ROW_HEIGHT)
    {
        let _ = body.append_child(&top);
    }
    for (position, row) in rows {
        row.set_hidden(false);
        let _ = row.set_attribute("aria-rowindex", &(position + 2).to_string());
        let _ = body.append_child(&row);
    }
    if end < count
        && let Some(bottom) = spacer((count - end) * ROW_HEIGHT)
    {
        let _ = body.append_child(&bottom);
    }
    if let Some(parent) = current.parent_node() {
        let _ = parent.replace_child(&body, &current);
    }
    port.set_scroll_top(top as i32);
    if let Some(active) = active.filter(|active| active.is_connected()) {
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = active.focus_with_options(&options);
    }
    app.borrow_mut().rendering = false;
}

pub fn reveal(app: &Shared, id: &str) {
    let state = app.borrow();
    let Some(position) = state
        .vm_indices
        .iter()
        .position(|&index| state.vms[index].id == id)
    else {
        return;
    };
    drop(state);
    let Some(port) = element("vm-scroll").and_then(|port| port.dyn_into::<HtmlElement>().ok())
    else {
        return;
    };
    let top = port.scroll_top().max(0) as usize;
    let height =
        (port.client_height().max(ROW_HEIGHT as i32) as usize).saturating_sub(HEADER_HEIGHT);
    let row_top = position * ROW_HEIGHT;
    if row_top < top {
        port.set_scroll_top(row_top as i32);
    } else if row_top + ROW_HEIGHT > top + height {
        port.set_scroll_top((row_top + ROW_HEIGHT - height) as i32);
    }
    app.borrow_mut().vm_scroll_top = port.scroll_top();
    render_window(app, false);
}

pub fn install(app: &Shared) -> Result<(), JsValue> {
    let scrolling = app.clone();
    let scroll = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(port) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlElement>().ok())
            .filter(|port| port.id() == "vm-scroll")
        else {
            return;
        };
        let mut state = scrolling.borrow_mut();
        if state.rendering {
            return;
        }
        state.vm_scroll_top = port.scroll_top();
        drop(state);
        render_window(&scrolling, false);
    });
    document().add_event_listener_with_callback_and_bool(
        "scroll",
        scroll.as_ref().unchecked_ref(),
        true,
    )?;
    scroll.forget();
    let resizing = app.clone();
    let resize = Closure::<dyn FnMut(Event)>::new(move |_| render_window(&resizing, false));
    window().add_event_listener_with_callback("resize", resize.as_ref().unchecked_ref())?;
    resize.forget();
    Ok(())
}
