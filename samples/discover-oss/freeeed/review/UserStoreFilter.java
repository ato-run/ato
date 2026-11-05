package org.freeeed.search.web.dao.user;

import java.io.ObjectInputFilter;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import org.freeeed.search.web.model.User;

/** Restricts the classes and graph size accepted in the local user-store prototype. */
final class UserStoreFilter implements ObjectInputFilter {
    public Status checkInput(FilterInfo info) {
        if (info.depth() > 16 || info.references() > 12000 || info.streamBytes() > 1048576
                || info.arrayLength() > 10000) return Status.REJECTED;
        Class<?> type = info.serialClass();
        if (type == null) return Status.UNDECIDED;
        while (type.isArray()) type = type.getComponentType();
        if (type.isPrimitive() || type == HashMap.class || type == HashSet.class
                || type == Map.Entry.class || type == String.class || type == Enum.class
                || type == Long.class || type == Number.class || type == User.class
                || type == User.Right.class) return Status.ALLOWED;
        return Status.REJECTED;
    }
}
