#!/usr/bin/env perl

use strict;
use warnings;
use FindBin qw($Bin);
use File::Temp qw(tempfile);
use JSON::PP qw(encode_json);
use Test::More;

require "$Bin/review_contract.pl";

my $valid_body = <<'MARKDOWN';
# Summary

## Review Contract

### Goal
Ensure every PR declares a stable review boundary.

### Non-goals
This does not evaluate whether the proposed design is correct.

### Accepted Residual Risks
The validator checks structure only; maintainers review semantics.

### Acceptance Criteria
- [ ] All required sections contain meaningful content.
- [ ] Required headings appear once and in order.

### Follow-ups
None — semantic automation remains a maintainer decision.
MARKDOWN

is_deeply(
    [ReviewContract::validate_review_contract($valid_body)],
    [],
    'accepts a complete contract',
);

my $missing = $valid_body;
$missing =~ s/### Non-goals\n//;
ok(
    grep($_ eq 'Missing required heading: ### Non-goals',
        ReviewContract::validate_review_contract($missing)),
    'rejects a missing heading',
);

my $empty = $valid_body;
$empty =~ s/Ensure every PR declares a stable review boundary\./<!-- Add the goal. -->\n- [ ]/;
ok(
    grep($_ eq 'Section has no meaningful content: ### Goal',
        ReviewContract::validate_review_contract($empty)),
    'comments and empty checkboxes are not content',
);

my $placeholder = $valid_body;
$placeholder =~ s/This does not evaluate whether the proposed design is correct\./TBD/;
ok(
    grep($_ eq 'Section has no meaningful content: ### Non-goals',
        ReviewContract::validate_review_contract($placeholder)),
    'rejects placeholder-only content',
);

my $duplicate = $valid_body;
$duplicate =~ s/### Goal/### Goal\nUseful text.\n### Goal/;
ok(
    grep($_ eq 'Heading must appear exactly once: ### Goal',
        ReviewContract::validate_review_contract($duplicate)),
    'rejects duplicate headings',
);

my $wrong_order = $valid_body;
$wrong_order =~ s{### Goal\nEnsure every PR declares a stable review boundary\.\n\n### Non-goals\nThis does not evaluate whether the proposed design is correct\.}{### Non-goals\nThis does not evaluate whether the proposed design is correct.\n\n### Goal\nEnsure every PR declares a stable review boundary.};
is_deeply(
    [ReviewContract::validate_review_contract($wrong_order)],
    ['Review Contract headings must appear in the required order'],
    'rejects headings in the wrong order',
);

my $bare_none = $valid_body;
$bare_none =~ s/None — semantic automation remains a maintainer decision\./None/;
ok(
    grep($_ eq 'Section has no meaningful content: ### Follow-ups',
        ReviewContract::validate_review_contract($bare_none)),
    'bare None requires a reason',
);

my $todo_variant = $valid_body;
$todo_variant =~ s/The validator checks structure only; maintainers review semantics\./TODO: explain this later/;
ok(
    grep($_ eq 'Section has no meaningful content: ### Accepted Residual Risks',
        ReviewContract::validate_review_contract($todo_variant)),
    'rejects TODO placeholder variants',
);

my $fenced = "```markdown\n$valid_body```\n";
ok(
    grep($_ eq 'Missing required heading: ## Review Contract',
        ReviewContract::validate_review_contract($fenced)),
    'headings inside a fenced code block do not count',
);

my $commented = "<!--\n$valid_body-->\n";
ok(
    grep($_ eq 'Missing required heading: ## Review Contract',
        ReviewContract::validate_review_contract($commented)),
    'headings inside an HTML comment do not count',
);

my $leaked_follow_up = $valid_body;
$leaked_follow_up =~ s/None — semantic automation remains a maintainer decision\./## Validation\nUnrelated content must not satisfy Follow-ups./;
ok(
    grep($_ eq 'Section has no meaningful content: ### Follow-ups',
        ReviewContract::validate_review_contract($leaked_follow_up)),
    'content from the next peer heading cannot satisfy a section',
);

my $indented_heading = $valid_body;
$indented_heading =~ s/^## Review Contract/    ## Review Contract/m;
ok(
    grep($_ eq 'Missing required heading: ## Review Contract',
        ReviewContract::validate_review_contract($indented_heading)),
    'four-space-indented pseudo-headings do not count',
);

for my $empty_value ('None —', '[ ]', '1. [ ]') {
    my $empty_follow_up = $valid_body;
    $empty_follow_up =~ s/None — semantic automation remains a maintainer decision\./$empty_value/;
    ok(
        grep($_ eq 'Section has no meaningful content: ### Follow-ups',
            ReviewContract::validate_review_contract($empty_follow_up)),
        "rejects reasonless or empty Follow-ups: $empty_value",
    );
}

my ($body_handle, $body_path) = tempfile();
print {$body_handle} $valid_body;
close $body_handle;
is(
    system($^X, "$Bin/review_contract.pl", '--body-file', $body_path),
    0,
    'CLI succeeds for a valid contract',
);
open $body_handle, '>', $body_path or die "Cannot rewrite $body_path: $!";
print {$body_handle} "## Review Contract\n";
close $body_handle;
isnt(
    system($^X, "$Bin/review_contract.pl", '--body-file', $body_path),
    0,
    'CLI fails for an invalid contract',
);

my ($event_handle, $event_path) = tempfile();
print {$event_handle} encode_json({
    pull_request => {
        body => '',
        labels => [{name => 'review-contract-exempt'}],
    },
});
close $event_handle;
my ($event_body, $exempt) = ReviewContract::load_event($event_path);
is($event_body, '', 'loads an empty event body');
ok($exempt, 'recognizes the maintainer exemption label');

done_testing();
