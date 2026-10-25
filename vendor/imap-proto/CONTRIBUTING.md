# Contributing

Thanks for considering helping this project. There are many ways you can help: using the library
and reporting bugs, reporting usability issues, making additions and improvements to the library,
documentation and finding security bugs.

## Reporting bugs

Please file a GitHub issue. Include as much information as possible. Suspected protocol bugs are
easier debugged with a pcap or reproduction steps.

Feel free to file GitHub issues to get help, or ask a question.

## AI policy

Using AI (LLMs) as tools for coding is welcome. A high bar is held for all contributions to this
project. Moreover, the project maintainers remain responsible for any code that is published as
part of a release. Contributors are expected to be responsible for any code they publish.

AI should not be used to generate comments when communicating with maintainers. Comments are
expected to be written by humans. Comments that are believed to be written by AI may be hidden
without notice.

If you are opening an issue, you should be able to describe the problem in your own words.

If you are opening a pull request, you are expected to be able to explain the proposed changes in
your own words. This includes the pull request body and responses to questions. Make sure you have
reviewed the PR yourself before submitting it for review to the maintainers. Do not copy responses
from the AI when replying to questions from maintainers.

If you wish to include context from an interaction with AI in your comments, it must be in a
quote block (using `>`) and disclosed as such. It must be accompanied by human commentary
explaining the relevance and implications of the context. Do not share long snippets.

AI is useful when communicating as a non-native English speaker. If you are using AI to edit your
comments for this purpose, please take the time to ensure it reflects your own voice and ideas.
When using AI for translation, we recommend writing in your native language and including the AI
translation in a quote block.

## Code changes

Some ideas and guidelines for contributions:

- For large features, file an issue prior to starting work.
  This means everyone can see what is in progress prior to a PR.
- Feel free to submit a PR even if the work is not totally finished, for early feedback.
- Prefer not to reference GitHub issue or PR numbers in commit messages.
- Try to keep code formatting commits separate from functional commits.

## Commit history

We prefer to keep the commit history clean and easy to follow. As such, we prefer small commits
that do one thing. In particular:

* Avoid mixing refactoring and functional changes in the same commit if possible
* Make mechanical changes (like renaming or moving code around) in a separate commit
* Isolate updates to `Cargo.lock` in their own commits

Our default workflow is to rebase clean commit history from a PR to `main`. As a result,
we ask that review feedback is addressed in the originating commit rather than in new commits,
by amending existing commits or squashing later commits back into the original commit(s).

## Testing

PRs which cause test failures or a significant coverage decrease are unlikely to be accepted.
