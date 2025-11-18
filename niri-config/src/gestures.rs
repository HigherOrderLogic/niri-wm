use std::str::FromStr;

use knuffel::errors::DecodeError;
use knuffel::traits::{Decode, ErrorSpan};
use miette::miette;

use crate::utils::{expect_only_children, MergeWith};
use crate::{Action, FloatOrInt, Modifiers};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Gestures {
    pub dnd_edge_view_scroll: DndEdgeViewScroll,
    pub dnd_edge_workspace_switch: DndEdgeWorkspaceSwitch,
    pub hot_corners: HotCorners,
    pub binds: Binds,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct GesturesPart {
    #[knuffel(child)]
    pub dnd_edge_view_scroll: Option<DndEdgeViewScrollPart>,
    #[knuffel(child)]
    pub dnd_edge_workspace_switch: Option<DndEdgeWorkspaceSwitchPart>,
    #[knuffel(child)]
    pub hot_corners: Option<HotCorners>,
    #[knuffel(child)]
    pub binds: Option<Binds>,
}

impl MergeWith<GesturesPart> for Gestures {
    fn merge_with(&mut self, part: &GesturesPart) {
        merge!(
            (self, part),
            dnd_edge_view_scroll,
            dnd_edge_workspace_switch,
            binds,
        );
        merge_clone!((self, part), hot_corners);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeViewScroll {
    pub trigger_width: f64,
    pub delay_ms: u16,
    pub max_speed: f64,
}

impl Default for DndEdgeViewScroll {
    fn default() -> Self {
        Self {
            trigger_width: 30., // Taken from GTK 4.
            delay_ms: 100,
            max_speed: 1500.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeViewScrollPart {
    #[knuffel(child, unwrap(argument))]
    pub trigger_width: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub delay_ms: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub max_speed: Option<FloatOrInt<0, 1_000_000>>,
}

impl MergeWith<DndEdgeViewScrollPart> for DndEdgeViewScroll {
    fn merge_with(&mut self, part: &DndEdgeViewScrollPart) {
        merge!((self, part), trigger_width, max_speed);
        merge_clone!((self, part), delay_ms);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeWorkspaceSwitch {
    pub trigger_height: f64,
    pub delay_ms: u16,
    pub max_speed: f64,
}

impl Default for DndEdgeWorkspaceSwitch {
    fn default() -> Self {
        Self {
            trigger_height: 50.,
            delay_ms: 100,
            max_speed: 1500.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeWorkspaceSwitchPart {
    #[knuffel(child, unwrap(argument))]
    pub trigger_height: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub delay_ms: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub max_speed: Option<FloatOrInt<0, 1_000_000>>,
}

impl MergeWith<DndEdgeWorkspaceSwitchPart> for DndEdgeWorkspaceSwitch {
    fn merge_with(&mut self, part: &DndEdgeWorkspaceSwitchPart) {
        merge!((self, part), trigger_height, max_speed);
        merge_clone!((self, part), delay_ms);
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct HotCorners {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub top_left: bool,
    #[knuffel(child)]
    pub top_right: bool,
    #[knuffel(child)]
    pub bottom_left: bool,
    #[knuffel(child)]
    pub bottom_right: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SwipeType {
    Any,
    Up,
    Down,
    Left,
    Right,
    Vertical,
    Horizontal,
}

impl FromStr for SwipeType {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Swipe" => Ok(Self::Any),
            "SwipeUp" => Ok(Self::Up),
            "SwipeDown" => Ok(Self::Down),
            "SwipeLeft" => Ok(Self::Left),
            "SwipeRight" => Ok(Self::Right),
            "SwipeVertical" => Ok(Self::Vertical),
            "SwipeHorizontal" => Ok(Self::Horizontal),
            unknown => Err(miette!("unknown swipe gesture type: {}", unknown)),
        }
    }
}

fn parse_swipe<S: ErrorSpan>(
    node: &knuffel::ast::SpannedNode<S>,
    s: &str,
) -> Result<(u8, SwipeType), DecodeError<S>> {
    let mut split = s.split('-');

    let fingers = split
        .next()
        .unwrap()
        .parse()
        .map_err(|e| DecodeError::conversion(&node.node_name, e))?;
    let swipe_type = split
        .next()
        .ok_or_else(|| DecodeError::missing(&node, "missing swipe gesture type"))?
        .parse()
        .map_err(|e: miette::Error| {
            DecodeError::conversion(&node.node_name, e.wrap_err("invalid swipe gesture type"))
        })?;

    Ok((fingers, swipe_type))
}

#[derive(Debug, Clone, PartialEq)]
pub enum PinchType {
    Any,
    In,
    Out,
}

impl FromStr for PinchType {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Pinch" => Ok(Self::Any),
            "PinchIn" => Ok(Self::In),
            "PinchOut" => Ok(Self::Out),
            unknown => Err(miette!("unknown pinch gesture type: {}", unknown)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Gesture {
    Swipe(u8, SwipeType),
    Pinch(PinchType),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bind {
    pub modifiers: Modifiers,
    pub gesture: Gesture,
    pub action: Action,
}

impl<S> Decode<S> for Bind
where
    S: ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, knuffel::errors::DecodeError<S>> {
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }

        for val in node.arguments.iter() {
            ctx.emit_error(DecodeError::unexpected(
                &val.literal,
                "argument",
                "no arguments expected for this node",
            ));
        }

        let mut parts = node.node_name.rsplitn(2, '+');
        let mut has_modifiers = false;

        let gesture = parts.next().unwrap();
        let modifiers = if let Some(s) = parts.next() {
            has_modifiers = true;
            s.parse().map_err(|e: miette::Error| {
                DecodeError::conversion(&node.node_name, e.wrap_err("invalid modifiers"))
            })?
        } else {
            Modifiers::empty()
        };
        let gesture = if has_modifiers {
            if gesture.chars().next().is_some_and(|c| c.is_numeric()) {
                let (fingers, swipe_type) = parse_swipe(node, gesture)?;
                Gesture::Swipe(fingers, swipe_type)
            } else {
                let pinch_type = gesture
                    .parse()
                    .map_err(|e| DecodeError::conversion(&node.node_name, e))?;
                Gesture::Pinch(pinch_type)
            }
        } else {
            if let Some(g) = gesture.strip_prefix("_") {
                let (fingers, swipe_type) = parse_swipe(node, g)?;
                Gesture::Swipe(fingers, swipe_type)
            } else {
                let pinch_type = gesture
                    .parse()
                    .map_err(|e| DecodeError::conversion(&node.node_name, e))?;
                Gesture::Pinch(pinch_type)
            }
        };

        let Some(child) = node.children().next() else {
            return Err(DecodeError::missing(
                node,
                "expected an action for this gesture bind",
            ));
        };

        let action = match Action::decode_node(child, ctx) {
            Ok(a) => a,
            Err(e) => {
                return Err(e);
            }
        };

        Ok(Self {
            gesture,
            modifiers,
            action,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Binds(pub Vec<Bind>);

impl<S> Decode<S> for Binds
where
    S: ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, knuffel::errors::DecodeError<S>> {
        expect_only_children(node, ctx);

        let mut binds = Vec::new();

        for child in node.children() {
            match Bind::decode_node(child, ctx) {
                Ok(bind) => binds.push(bind),
                Err(e) => ctx.emit_error(e),
            }
        }

        Ok(Self(binds))
    }
}

impl MergeWith<Binds> for Binds {
    fn merge_with(&mut self, part: &Binds) {
        self.0.extend(part.0.clone());
    }
}
