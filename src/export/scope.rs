use crate::error::{AppError, Result};
use crate::report::ExportCounters;
use crate::types::{CheckpointState, ExportOptions, ScannedMessage};

pub(crate) fn empty_checkpoint(chat_id: i64) -> CheckpointState {
    CheckpointState {
        chat_id,
        high_watermark_message_id: None,
        backfill_cursor_message_id: None,
        backfill_complete: false,
    }
}

pub(crate) fn reached_limit(options: &ExportOptions, counters: &ExportCounters) -> bool {
    options
        .limit
        .is_some_and(|limit| counters.scanned_messages >= limit)
}

pub(crate) fn validate_export_options(options: &ExportOptions) -> Result<()> {
    if let (Some(since_id), Some(until_id)) = (options.since_id, options.until_id)
        && since_id > until_id
    {
        return Err(AppError::InvalidArgument(
            "--since-id must be lower than or equal to --until-id".to_string(),
        ));
    }
    if let (Some(date_from), Some(date_to)) = (options.date_from, options.date_to)
        && date_from > date_to
    {
        return Err(AppError::InvalidArgument(
            "--date-from must be earlier than or equal to --date-to".to_string(),
        ));
    }
    if options.resume && options.rescan {
        return Err(AppError::InvalidArgument(
            "--resume and --rescan cannot be used together".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn within_scope<M>(message: &ScannedMessage<M>, options: &ExportOptions) -> bool {
    if let Some(until_id) = options.until_id
        && message.message_id > until_id
    {
        return false;
    }
    if let Some(since_id) = options.since_id
        && message.message_id < since_id
    {
        return false;
    }
    if let Some(date_to) = options.date_to
        && message.date.date_naive() > date_to
    {
        return false;
    }
    if let Some(date_from) = options.date_from
        && message.date.date_naive() < date_from
    {
        return false;
    }
    true
}

pub(crate) fn should_stop_on_message<M>(
    message: &ScannedMessage<M>,
    options: &ExportOptions,
) -> bool {
    if let Some(since_id) = options.since_id
        && message.message_id < since_id
    {
        return true;
    }
    if let Some(date_from) = options.date_from
        && message.date.date_naive() < date_from
    {
        return true;
    }
    false
}
