"""Example python module 4 for indexing tests."""

def mutate_obj_4(obj):
    obj = obj + 4
    return obj

class Worker4:
    def apply(self, value):
        return mutate_obj_4(value)
