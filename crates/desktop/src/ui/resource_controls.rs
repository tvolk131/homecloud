use super::*;
use crate::resources::FormBounds;
use iced::widget::{column, row};

pub(super) fn controls(app: &App) -> Element<'_, Msg> {
    let Some(bounds) = FormBounds::displayed(app) else {
        return copy("Resource capacity unavailable.");
    };
    let grant = bounds.is_grant();
    let fields = if grant {
        [Field::RamGrant, Field::CoreGrant, Field::DiskGrant]
    } else {
        [Field::Ram, Field::Cores, Field::Disk]
    };
    controls_with(app, fields, fields.map(|field| bounds.for_field(field)))
}

pub(super) fn controls_with(
    app: &App,
    fields: [Field; 3],
    bounds: [crate::resources::Bound; 3],
) -> Element<'_, Msg> {
    let mut controls = column![].spacing(8);
    for ((field, label, key), bound) in [
        (fields[0], "Memory", "memory"),
        (fields[1], "Cores", "cores"),
        (
            fields[2],
            if fields[2] == Field::ReserveDisk {
                "Pool floor"
            } else {
                "Disk"
            },
            "disk",
        ),
    ]
    .into_iter()
    .zip(bounds)
    {
        let cpu = matches!(field, Field::Cores | Field::CoreGrant | Field::ReserveCores);
        let unit = if cpu { "cores" } else { "GiB" };
        let range = if bound.max < bound.min {
            "Unavailable".into()
        } else if bound.min == bound.max {
            format!(
                "{} {}",
                bound.min,
                if cpu && bound.min == 1 { "core" } else { unit }
            )
        } else {
            format!("{}–{} {unit}", bound.min, bound.max)
        };
        let disabled = app.busy || app.snapshot_unavailable;
        let value = app
            .get(field)
            .parse::<u64>()
            .unwrap_or(bound.min)
            .clamp(bound.min, bound.max.max(bound.min));
        let slider = iced_m3::slider(
            bound.min as f32..=bound.max.max(bound.min) as f32,
            value as f32,
        )
        .step(1.0)
        .ticks(bound.max.saturating_sub(bound.min) <= 16)
        .on_change(move |value| Msg::ResourceValue(field, value.round() as u64))
        .disabled(disabled || bound.max <= bound.min);
        let number = iced_m3::text_field(unit, app.get(field))
            .id(format!("resource-value-{key}"))
            .on_input(move |value| Msg::Edit(field, value))
            .width(104)
            .disabled(disabled);
        controls = controls.push(
            row![
                container(column![typography(label, Type::TitleMedium), copy(range)].spacing(4))
                    .width(96),
                container(slider)
                    .id(format!("resource-slider-{key}"))
                    .width(Length::Fill),
                number,
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    controls.into()
}

pub(super) fn validation(app: &App, fallback: String) -> Element<'_, Msg> {
    let error = FormBounds::displayed(app).and_then(|bounds| bounds.validate(app).err());
    supporting(error.clone().unwrap_or(fallback), error.is_some())
}

pub(super) fn supporting(message: String, error: bool) -> Element<'static, Msg> {
    container(
        typography(message, Type::BodyMedium).style(move |theme: &Theme| text::Style {
            color: Some(if error {
                theme.colors.error
            } else {
                theme.colors.on_surface_variant
            }),
        }),
    )
    .height(44)
    .width(Length::Fill)
    .into()
}
