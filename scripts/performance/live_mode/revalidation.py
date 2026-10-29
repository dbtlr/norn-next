"""Shared complete P2 and canonical suffix oracles for public measurement."""

COMMANDS = ({'op':'get','target':'small','whole':True}, {'op':'find'})
COUNTERS = {'snapshots_opened','statements_executed','vm_steps','full_scan_steps'}
GET_WORK = {'get_statements','get_full_scan_steps','get_sorts','get_vm_steps','get_anchor_headings','get_link_candidates_read'}
FIND_WORK = {'find_statements','find_keys_read','find_page_full_scan_steps','find_page_sorts','find_page_vm_steps',
             'find_documents_hydrated','find_tag_rows','find_heading_rows','find_block_rows','find_link_rows',
             'find_link_candidates_read','find_finding_rows'}


def integer(value):
    if type(value) is not int or value < 0:
        raise ValueError('invalid nonnegative receipt integer')
    return value


def root_identity(root):
    vault=(root/'vault').stat()
    return f'{vault.st_dev:016x}{vault.st_ino:016x}'


def counters(value, *, executed):
    if not isinstance(value,dict) or set(value)!=COUNTERS:
        raise ValueError('missing actual snapshot counters')
    for key,v in value.items():
        integer(v)
        if key!='full_scan_steps' and executed and v==0:
            raise ValueError('executed builder requires actual SQL work')
    if value['snapshots_opened']!=1:
        raise ValueError('one actual snapshot required')


def content_answer(index,response,manifest,*,first_index=False):
    """Shared full content oracle; envelope provenance is validated by the caller."""
    answer=response['answer']
    if index==0:
        from results import validate_first_get_content
        normalized=validate_first_get_content(response,manifest,first_index=first_index)
    else:
        from results import validate_read
        from results import metadata_answer
        validate_read('current',COMMANDS[1],response,manifest)
        normalized=metadata_answer(response)
        normalized.pop('reading',None)
        fold=str.maketrans('ABCDEFGHIJKLMNOPQRSTUVWXYZ','abcdefghijklmnopqrstuvwxyz')
        ordered=sorted(manifest['active_paths'],key=lambda p:(p.translate(fold),p))
        report=answer['report'];page=report.get('page',{})
        if (set(report)!={'page','rule_sets'} or report['rule_sets']!=[]
                or set(page)!={'rows','next','moved'} or page['moved']!=[]
                or page['rows']!=[dict(path=p) for p in ordered[:25]]
                or (page['next'] is not None)!=(len(ordered)>25)
                or answer['unsatisfied']!=[] or answer['advisories']!=[]):
            raise ValueError('full Find differs from authored complete page oracle')
        if page['next'] is not None:
            cursor=normalized['report']['page']['next']
            key=dict(row='document',order=dict(key=dict(by='path'),direction='ascending'),sort=None,path=ordered[24])
            if (set(cursor)!={'snapshot','key'} or cursor['key']!=key
                    or cursor['snapshot']!=dict(schema_fingerprint=None,sidecar_revision=None)):
                raise ValueError('Find cursor position/order or provenance differs')
    return normalized
