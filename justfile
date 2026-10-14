# The front door of a checkout. The steps live in bin/, the versions in tool-versions.env.

# Install and set up every development tool, then report what is missing or drifted
setup:
    bash bin/setup dev
    chock doctor

# Only what `chock run` needs, as CI installs it
setup-gates:
    bash bin/setup gates
