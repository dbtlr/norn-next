"""Two logical operator requests; private observer dispatch is explicitly distinct."""
import copy

from loading import metadata_answer
from results import validate_first_get, validate_read

COMMANDS = ({'op': 'get', 'target': 'small', 'whole': True}, {'op': 'find'})
STAGES = (('canonical-demand', 'whole_slug'), ('canonical-intake', 'active_page'))
SCOPE = 'private simulated capability availability'


def integer(value):
    if type(value) is not int or value < 0:
        raise ValueError('invalid nonnegative clock or provenance integer')
    return value


def validate_answer(index, response, manifest, *, private=False):
    if type(index) is not int or index not in (0, 1):
        raise ValueError('only Get and one basic Find are authorized')
    if index == 0:
        return validate_first_get(response, manifest, private=private)
    answer = response.get('answer', {})
    if answer.get('unsatisfied') != [] or answer.get('advisories') != []:
        raise ValueError('basic Find must be fully satisfied')
    stage = copy.deepcopy(response)
    if private:
        if (stage.get('simulation') != 'paused' or stage.get('global_ready') is not False
                or 'reading' in answer):
            raise ValueError('invalid private Find provenance')
        epoch, generation = stage.get('projection_epoch'), stage.get('projection_generation')
    else:
        reading = answer.get('reading', {})
        if reading.get('trust') != {'state': 'ready'}:
            raise ValueError('native Find provenance is not Ready')
        epoch, generation = reading.get('epoch'), reading.get('generation')
        stage.update(projection_epoch=epoch, projection_generation=generation)
        stage['answer'].pop('reading', None)
    if not isinstance(epoch, str) or not epoch:
        raise ValueError('missing Find epoch provenance')
    integer(generation)
    validate_read('current', COMMANDS[1], response, manifest)
    return metadata_answer(stage)


def validate_history(stages, ready, *, parent_ready_received_ns):
    if len(stages) != 2:
        raise ValueError('exactly two private prefix queries required; no final probes')
    previous_worker, previous_parent = 0, 0
    for index, (event, (stage, probe), command) in enumerate(zip(stages, STAGES, COMMANDS), 1):
        if (event.get('startup_stage') != stage or event.get('probe') != probe
                or event.get('command') != command or event.get('global_ready') is not False
                or event.get('simulation') != 'paused' or event.get('availability_scope') != SCOPE
                or type(event.get('operator_prefix_sequence')) is not int
                or event['operator_prefix_sequence'] != index or event.get('verified') is not True):
            raise ValueError('invalid operator prefix request history')
        # Each comparison is within a single process clock domain.
        boundary, started, finished = (integer(event.get(key)) for key in
                                      ('stage_boundary_ns', 'query_started_ns', 'query_finished_ns'))
        integer(event.get('query_host_ns'))
        if not previous_worker <= boundary <= started <= finished <= integer(ready['attach_ns']) <= integer(ready['startup_complete_ns']):
            raise ValueError('private worker query outside its pre-Ready clock bounds')
        received, validated, acknowledged = (integer(event.get(key)) for key in
            ('parent_received_ns', 'parent_validated_ns', 'parent_ack_published_ns'))
        if not previous_parent <= received <= validated <= acknowledged <= integer(parent_ready_received_ns):
            raise ValueError('parent availability endpoint outside acknowledged pre-Ready bounds')
        previous_worker, previous_parent = finished, acknowledged
