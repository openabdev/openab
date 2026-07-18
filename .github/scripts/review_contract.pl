#!/usr/bin/env perl

package ReviewContract;

use strict;
use warnings;
use Getopt::Long qw(GetOptionsFromArray);
use JSON::PP qw(decode_json);

our $CONTRACT_TITLE = '## Review Contract';
our @SECTION_HEADINGS = (
    '### Goal',
    '### Non-goals',
    '### Accepted Residual Risks',
    '### Acceptance Criteria',
    '### Follow-ups',
);
our @REQUIRED_HEADINGS = ($CONTRACT_TITLE, @SECTION_HEADINGS);
our $EXEMPT_LABEL = 'review-contract-exempt';

sub _trim {
    my ($value) = @_;
    $value =~ s/^\s+|\s+$//g;
    return $value;
}

sub _required_heading_matches {
    my ($line, $heading) = @_;
    return $line =~ /^ {0,3}\Q$heading\E\s*$/;
}

sub _visible_lines {
    my ($body) = @_;
    $body =~ s/<!--.*?(?:-->|$)//gs;

    my @visible;
    my ($fence_character, $fence_length);
    for my $line (split /\n/, $body, -1) {
        if (defined $fence_character) {
            my $required = $fence_character x $fence_length;
            if ($line =~ /^ {0,3}\Q$required\E\Q$fence_character\E*\s*$/) {
                undef $fence_character;
                undef $fence_length;
            }
            push @visible, '';
            next;
        }

        if ($line =~ /^ {0,3}(`{3,}|~{3,})/) {
            $fence_character = substr($1, 0, 1);
            $fence_length = length $1;
            push @visible, '';
            next;
        }

        push @visible, $line;
    }
    return @visible;
}

sub _meaningful_content {
    my ($raw) = @_;
    my @meaningful;
    for my $line (split /\n/, $raw) {
        my $stripped = _trim($line);
        next if $stripped eq '' || $stripped =~ /^[-*+]$/;
        next if $stripped =~ /^(?:(?:[-*+]|\d+[.)])\s*)?\[[ xX]\]\s*(?:\.\.\.)?$/i;
        push @meaningful, $stripped;
    }

    return 0 unless @meaningful;
    my $combined = join ' ', @meaningful;
    return 0 if $combined =~ /^(?:tbd|todo)\b/i;
    return 0 if $combined =~ /^(?:none|n\/?a|not applicable)(?:[.!]|\s*(?:[-:]|\x{2014}|\xE2\x80\x94)\s*)?$/i;
    return 0 if $combined =~ /^(?:fill (?:this|me) in|your (?:text|answer) here|\.\.\.)[.!]?$/i;
    return 1;
}

sub validate_review_contract {
    my ($body) = @_;
    my @lines = _visible_lines($body);
    my %positions;
    my @errors;

    for my $heading (@REQUIRED_HEADINGS) {
        my @matches = grep {
            _required_heading_matches($lines[$_], $heading)
        } 0 .. $#lines;
        if (!@matches) {
            push @errors, "Missing required heading: $heading";
        } elsif (@matches > 1) {
            push @errors, "Heading must appear exactly once: $heading";
        } else {
            $positions{$heading} = $matches[0];
        }
    }

    return @errors if @errors;

    my @ordered = map { $positions{$_} } @REQUIRED_HEADINGS;
    for my $index (1 .. $#ordered) {
        if ($ordered[$index] < $ordered[$index - 1]) {
            return ('Review Contract headings must appear in the required order');
        }
    }

    for my $index (0 .. $#SECTION_HEADINGS) {
        my $heading = $SECTION_HEADINGS[$index];
        my $start = $positions{$heading} + 1;
        my $limit = $index < $#SECTION_HEADINGS
            ? $positions{$SECTION_HEADINGS[$index + 1]} - 1
            : $#lines;
        my $end = $limit;
        for my $candidate ($start .. $limit) {
            if ($lines[$candidate] =~ /^ {0,3}#{1,3}\s+/) {
                $end = $candidate - 1;
                last;
            }
        }
        my $content = $start <= $end ? join("\n", @lines[$start .. $end]) : '';
        push @errors, "Section has no meaningful content: $heading"
            unless _meaningful_content($content);
    }

    return @errors;
}

sub _read_file {
    my ($path) = @_;
    open my $handle, '<:raw', $path or die "Cannot read $path: $!\n";
    local $/;
    return <$handle>;
}

sub load_event {
    my ($path) = @_;
    my $payload = decode_json(_read_file($path));
    my $pr = ref $payload->{pull_request} eq 'HASH' ? $payload->{pull_request} : {};
    my $body = defined $pr->{body} ? $pr->{body} : '';
    my $labels = ref $pr->{labels} eq 'ARRAY' ? $pr->{labels} : [];
    my $exempt = grep {
        ref $_ eq 'HASH' && defined $_->{name} && $_->{name} eq $EXEMPT_LABEL
    } @$labels;
    return ($body, $exempt ? 1 : 0);
}

sub main {
    my (@args) = @_;
    my ($event, $body_file);
    GetOptionsFromArray(
        \@args,
        'event=s' => \$event,
        'body-file=s' => \$body_file,
    ) or die "Usage: $0 (--event FILE | --body-file FILE|-)\n";
    die "Usage: $0 (--event FILE | --body-file FILE|-)\n"
        if (defined $event) == (defined $body_file);

    my ($body, $exempt);
    if (defined $event) {
        ($body, $exempt) = load_event($event);
    } elsif ($body_file eq '-') {
        local $/;
        $body = <STDIN>;
        $exempt = 0;
    } else {
        $body = _read_file($body_file);
        $exempt = 0;
    }

    if ($exempt) {
        print "Review Contract validation exempted by label: $EXEMPT_LABEL\n";
        return 0;
    }

    my @errors = validate_review_contract($body);
    if (@errors) {
        print "Review Contract validation failed:\n";
        print "- $_\n" for @errors;
        return 1;
    }

    print "Review Contract validation passed\n";
    return 0;
}

exit main(@ARGV) unless caller;
1;
