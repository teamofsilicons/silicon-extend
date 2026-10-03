/** Current application reference overlays; protected historical requirements stay unchanged. */
export function applyOrganizationReference(cli, groups) {
  const rewrite = (prefix, summary, extra = {}) => {
    const command = cli.commands.find(c => c.usage.startsWith(prefix));
    if (!command) throw new Error(`Missing reference command ${prefix}`);
    // Old examples and notes sometimes described authority across all organizations.
    for (const key of ['summary', 'takes', 'gives', 'notes', 'behaviour']) delete command[key];
    Object.assign(command, { summary }, extra);
  };
  const add = (prefix, command) => {
    const group = groups.find(g => g.usages.some(u => u.startsWith(prefix)));
    if (!group) throw new Error(`Missing reference group ${prefix}`);
    cli.commands.push(command);
    group.usages.push(command.usage);
  };
  cli.global_flags.find(f => f.flag === '--team <handle>').gives = 'Selects this account’s separately saved organization login for one command. All resources and mutations remain in that organization; a missing saved context requires login. The default selection is unchanged.';
  cli.state_files = [
    '{home}/.extend/auth.json — selected organization login (0600)',
    '{home}/.extend/contexts/<world>/<context>.json — separate tokens for each API origin, account and organization (0600)',
    '{home}/.extend/sessions/<context>/ — connected session and device cache for this API origin, account and organization (plus a test environment subdirectory when testing)',
    '{home}/.extend/config.toml — CLI settings',
    '{home}/.extend/test/<test_id>.json — test environment and its selected login',
    '{default home}/.extend/home — state directory override',
  ];
  rewrite('extend login <slt>', 'Save a login for one account and organization selected in IAM. Earlier organization logins remain available. Legacy unscoped credentials require a fresh login.');
  rewrite('extend logout', 'Sign out of the selected organization and remove its saved login. Sessions authorized by that login end in this organization. Other saved contexts and physical device configuration remain available.');
  rewrite('extend team ls', 'List the selected login’s organization, or restore a separately saved organization login for this account with team use.');
  rewrite('extend team silicons', 'List Silicons in the selected organization. --all-teams preserves the directory metadata response format but cannot broaden the saved IAM context.');
  add('extend login ', { usage: 'extend login contexts', who: 'both', summary: 'List saved accounts and organizations without exposing tokens.' });
  add('extend login ', { usage: 'extend login use <member_id> <org>', who: 'both', summary: 'Restore the saved account and organization, including its own connected session cache.' });
  rewrite('extend device ls ', 'List this organization’s devices. Owners use their own bindings; Silicons see explicitly granted devices, including hidden devices. --team-visible lists other members’ shared devices for discovery. --removed includes your removed bindings.', { takes: {'--team-visible':'Show organization-visible devices shared by colleagues.'} });
  rewrite('extend device pair ', 'Pair a device into this organization. New devices default to organization-visible. Use --visibility personal to hide discovery from other Carbons and ungranted Silicons; Silicon control always requires an explicit grant.', {takes:{'--visibility':'team (default, organization-visible) or personal (hidden except for the owner and explicitly granted same-organization Silicons)','--access':'Silicon id in this organization to grant control to, including for a hidden device.','--ttl-days':'Pair expiry in days, 1–30.'}});
  rewrite('extend device attach ', 'Configure an attached device using your host in the selected organization. Organization-visible by default; --visibility personal hides it except from the owner and explicitly granted same-organization Silicons.', {takes:{'--visibility':'team (default) or personal'}});
  rewrite('extend device visibility ', 'Set this organization binding to personal (hidden from other Carbons and ungranted Silicons) or team (discoverable by members). Explicitly granted same-organization Silicons retain access and active sessions when hidden.');
  rewrite('extend device stop ', 'Stop sessions you own or authorized through your devices in this organization, including your carried devices. Other organizations and other owners cannot be stopped through this route. The physical device can still stop its active session.');
  rewrite('extend device rm ', 'Remove the device and its carried-device bindings from this organization. Their physical setup and other organization bindings remain. Import a configured device to add it again.');
  rewrite('extend device access ls ', 'List the owner’s Silicon grants in this organization.');
  rewrite('extend device access grant ', 'Grant a Silicon control of your device in the selected organization, whether organization-visible or hidden. Hidden devices stay inaccessible to other Carbons and Silicons without an explicit grant.');
  rewrite('extend device access revoke ', 'Remove the Silicon’s grant and end its sessions in the selected organization. An explicit --team must select a separately saved context.');
  rewrite('extend device wake-requests ', 'Read or answer requests in this organization. Mute settings and explicit Silicon filters affect this organization’s binding only. Physical native wake signals still describe the device itself.');
  rewrite('extend ting status ', 'Read or restore your recipient registration in the selected organization. --all-teams remains a compatibility spelling and cannot broaden the IAM context. Missing app types require the owning organization’s Ting manager.');
  rewrite('extend session ls ', 'List your sessions (Silicon), or sessions through your own devices (Carbon), in the selected organization.');
  rewrite('extend file ls ', 'List files from your sessions (Silicon), or your devices (Carbon), in the selected organization. Downloads require that organization’s context.');
  add('extend device ls ', { usage: 'extend device importable', who: 'carbon', summary: 'List your configured devices that are not already bound to this organization, without exposing their source organizations.', api: 'GET /api/v1/devices/importable' });
  add('extend device ls ', { usage: 'extend device import <device_id> [--visibility personal|team] [--key <uuid>]', who: 'carbon', summary: 'Import your configured device into the selected organization without pairing again. Defaults to organization-visible. Use --visibility personal to hide it except from the owner and explicitly granted same-organization Silicons. Reuse the same UUID key and visibility after an uncertain response. An attached device imports a missing host with the same chosen visibility; existing active host visibility is preserved.', api: 'POST /api/v1/devices/{device_id}/import' });
}
