"""Example python module 1 for indexing tests."""

def mutate_obj_1(obj):
    obj = obj + 1
    return obj

class Worker1:
    def apply(self, value):
        return mutate_obj_1(value)
