// Shared process gates for retained AX handles, also exercised by unit-check.
#pragma once
#include <stdbool.h>

// Events target the app; AX handles must retain their registration-time host.
static inline bool IPURElementProcessIsLive(int appPID, int axPID, int currentAXPID)
{
  return appPID > 0 && axPID > 0 && currentAXPID == axPID;
}

static inline bool IPURFocusHitMatches(int axPID, int hitAXPID, bool sameElement)
{
  return axPID > 0 && hitAXPID == axPID && sameElement;
}
