"""Recheck retained native warm responses using the accepted health-once oracles."""
import base64
import json



def nonnegative_generation(value):
    if type(value) is not int or not 0 <= value <= 2**64 - 1:
        raise ValueError('missing or invalid provenance generation')
    return value


def provenance_epoch(value):
    # AnswerReading carries an opaque String, not a fixed-width hex number.
    if not isinstance(value, str) or not value:
        raise ValueError('missing or invalid provenance epoch')
    return value


def validate_cursor_provenance(report, epoch, generation, index):
    page = report.get('page')
    if page is None:
        return
    if not isinstance(page, dict):
        raise ValueError(f'invalid native page at response {index}')
    token = page.get('next')
    if token is None:
        return
    try:
        if not isinstance(token, str) or not token:
            raise ValueError('invalid cursor token')
        cursor = json.loads(base64.b64decode(token + '=' * (-len(token) % 4),
                                            altchars=b'-_', validate=True))
        snapshot = cursor['snapshot']
        if (provenance_epoch(snapshot['epoch']) != epoch
                or nonnegative_generation(snapshot['generation']) != generation):
            raise ValueError('cursor snapshot differs from answer reading')
    except (ValueError, TypeError, KeyError) as problem:
        raise ValueError(f'invalid cursor provenance at response {index}') from problem
