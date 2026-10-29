"""Allowlisted account display data; credentials and raw provider replies stay local."""
import unicodedata


def display_text(value, maximum=320):
    if not isinstance(value, str) or not value.strip() or len(value) > maximum:
        return None
    if any(unicodedata.category(char).startswith('C') for char in value):
        return None
    return value.strip()


def codex_account(value):
    if not isinstance(value, dict): return {'state': 'unknown'}
    account = value.get('account')
    if account is None:
        return {'state': 'signed_out'}
    if not isinstance(account, dict) or account.get('type') not in ('chatgpt', 'apiKey'):
        return {'state': 'unknown'}
    routing = value.get('workspaceRouting') or {}
    return {'state': 'signed_in', 'email': display_text(account.get('email')),
            'account_id': display_text(routing.get('chatgptAccountId'), 128) if isinstance(routing, dict) else None,
            'plan': display_text(account.get('planType'), 80),
            'authentication': account['type']}


def claude_account(value, returncode):
    if not isinstance(value, dict): return {'state': 'unknown'}
    if returncode == 1 and value.get('loggedIn') is False:
        return {'state': 'signed_out'}
    if returncode != 0 or value.get('loggedIn') is not True:
        return {'state': 'unknown'}
    return {'state': 'signed_in', 'email': display_text(value.get('email')),
            'plan': display_text(value.get('subscriptionType'), 80),
            'authentication': display_text(value.get('authMethod'), 40)}
